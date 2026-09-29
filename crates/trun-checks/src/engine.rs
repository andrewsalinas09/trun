//! The Starlark check engine (docs/05-checks.md, D19).
//!
//! A check file is compiled once into a frozen module (top level: `META`, helper
//! functions). Each evaluation calls its `check(run)` on a fresh temporary heap with
//! a time budget, and collects the action calls (`warn`, `stalled`, `fail`, …).
//!
//! Durations are **seconds** everywhere in the check API: `mins(15)` is 900,
//! `run.elapsed()` and `series.age()` are seconds, `slope()`/`rate()` are per second.

use crate::data::{RunData, Series};
use allocative::Allocative;
use starlark::environment::{
    FrozenModule, Globals, GlobalsBuilder, Methods, MethodsBuilder, Module,
};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::dict::{AllocDict, DictRef};
use starlark::values::float::StarlarkFloat;
use starlark::values::list::AllocList;
use starlark::values::none::{NoneOr, NoneType};
use starlark::values::{Heap, NoSerialize, ProvidesStaticType, StarlarkValue, Value, ValueLike};
use starlark::{starlark_module, starlark_simple_value};
use starlark_derive::{StarlarkPagablePanic, starlark_value};
use std::cell::RefCell;
use std::fmt;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use trun_proto::Millis;

/// Per-evaluation time budget. Starlark has no `while`/recursion, so only huge
/// `for` loops can get close.
pub const EVAL_BUDGET: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Info,
    Warn,
    Stalled,
    Fail,
    /// Cancel the run now.
    Kill,
    /// Send a desktop/phone notification (no alert).
    Notify,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub kind: ActionKind,
    pub message: String,
    pub key: Option<String>,
}

thread_local! {
    static ACTIONS: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
}

fn record(kind: ActionKind, message: &str, key: Option<&str>) {
    ACTIONS.with(|a| {
        a.borrow_mut().push(Action {
            kind,
            message: message.to_string(),
            key: key.map(str::to_string),
        })
    });
}

/// A check file.
#[derive(Debug, Clone)]
pub struct CheckSource {
    /// File stem (`defaults`, `training`); also the override key.
    pub name: String,
    /// Where it came from (a path, or `builtin`), for error messages.
    pub origin: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Meta {
    /// Glob on the run name; `None` = all runs.
    pub applies: Option<String>,
    /// Seconds between evaluations.
    pub every: f64,
    /// Don't evaluate until the run is this many seconds old.
    pub grace: f64,
    /// `fail()` also cancels the run.
    pub kill: bool,
    /// Consecutive non-firing evaluations before an alert clears.
    pub clear_after: u32,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            applies: None,
            every: 10.0,
            grace: 0.0,
            kill: false,
            clear_after: 3,
        }
    }
}

pub struct Compiled {
    pub name: String,
    pub origin: String,
    pub meta: Meta,
    module: FrozenModule,
}

impl fmt::Debug for Compiled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Compiled")
            .field("name", &self.name)
            .field("meta", &self.meta)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Starlark values
// ---------------------------------------------------------------------------

/// Borrowed view of a [`Series`] for one evaluation.
///
/// SAFETY: values of this type only live on the temporary heap of a single
/// `Engine::evaluate` call, which borrows the `RunData` they point into for its
/// whole duration; the heap (and every value on it) is dropped before it returns,
/// and check code cannot stash values anywhere longer-lived (the module is frozen).
#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
struct SSeries {
    #[allocative(skip)]
    ptr: *const Series,
    now: Millis,
}
unsafe impl Send for SSeries {}
unsafe impl Sync for SSeries {}
starlark_simple_value!(SSeries);

impl SSeries {
    fn get(&self) -> &Series {
        unsafe { &*self.ptr }
    }
}

impl fmt::Display for SSeries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<series of {} points>", self.get().len())
    }
}

#[starlark_value(type = "series")]
impl<'v> StarlarkValue<'v> for SSeries {
    fn get_methods() -> Option<&'static Methods> {
        Some(SERIES_METHODS.methods())
    }
}

static EMPTY: LazyLock<Series> = LazyLock::new(Series::default);

fn opt(v: Option<f64>) -> NoneOr<f64> {
    match v {
        Some(x) => NoneOr::Other(x),
        None => NoneOr::None,
    }
}

fn to_f64(v: Value) -> Option<f64> {
    if let Some(f) = v.downcast_ref::<StarlarkFloat>() {
        return Some(f.0);
    }
    v.unpack_i32().map(|i| i as f64)
}

fn window(w: Option<Value>) -> anyhow::Result<Option<f64>> {
    match w {
        None => Ok(None),
        Some(v) if v.is_none() => Ok(None),
        Some(v) => to_f64(v).map(Some).ok_or_else(|| {
            anyhow::anyhow!("window must be a number of seconds, got {}", v.get_type())
        }),
    }
}

#[starlark_module]
fn series_methods(builder: &mut MethodsBuilder) {
    /// Latest value (may be NaN), or None if the series is empty.
    fn last(this: &SSeries) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().last(this.now)))
    }
    fn avg<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().avg(this.now, self::window(window)?)))
    }
    fn min<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().min(this.now, self::window(window)?)))
    }
    fn max<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().max(this.now, self::window(window)?)))
    }
    fn stddev<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().stddev(this.now, self::window(window)?)))
    }
    /// Least-squares slope, per second.
    fn slope<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().slope(this.now, self::window(window)?)))
    }
    fn delta<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().delta(this.now, self::window(window)?)))
    }
    /// Change per second, first to last point in the window.
    fn rate<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().rate(this.now, self::window(window)?)))
    }
    fn count<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<i32> {
        Ok(this.get().count(this.now, self::window(window)?) as i32)
    }
    /// Number of NaN/inf values in the window.
    fn count_non_finite<'v>(this: &SSeries, window: Option<Value<'v>>) -> anyhow::Result<i32> {
        Ok(this.get().count_non_finite(this.now, self::window(window)?) as i32)
    }
    /// Seconds since the latest point, or None if empty.
    fn age(this: &SSeries) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().age(this.now)))
    }
    fn exists(this: &SSeries) -> anyhow::Result<bool> {
        Ok(!this.get().is_empty())
    }
}
starlark::methods_static!(SERIES_METHODS = series_methods);

/// Borrowed view of a [`RunData`]; same safety argument as [`SSeries`].
#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
struct SRun {
    #[allocative(skip)]
    ptr: *const RunData,
    now: Millis,
}
unsafe impl Send for SRun {}
unsafe impl Sync for SRun {}
starlark_simple_value!(SRun);

impl SRun {
    fn get(&self) -> &RunData {
        unsafe { &*self.ptr }
    }

    fn series(&self, s: &Series) -> SSeries {
        SSeries {
            ptr: s as *const Series,
            now: self.now,
        }
    }
}

impl fmt::Display for SRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<run {} {:?}>", self.get().id, self.get().name)
    }
}

#[starlark_value(type = "run")]
impl<'v> StarlarkValue<'v> for SRun {
    fn get_methods() -> Option<&'static Methods> {
        Some(RUN_METHODS.methods())
    }
}

#[starlark_module]
fn run_methods(builder: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn id(this: &SRun) -> anyhow::Result<String> {
        Ok(this.get().id.clone())
    }
    #[starlark(attribute)]
    fn name(this: &SRun) -> anyhow::Result<String> {
        Ok(this.get().name.clone())
    }
    #[starlark(attribute)]
    fn project(this: &SRun) -> anyhow::Result<String> {
        Ok(this.get().project.clone())
    }
    /// queued | starting | running | succeeded | failed | cancelled | lost | preempted
    #[starlark(attribute)]
    fn lifecycle(this: &SRun) -> anyhow::Result<String> {
        Ok(this.get().lifecycle.as_str().to_string())
    }
    /// Seconds since the run started (0 before it starts).
    fn elapsed(this: &SRun) -> anyhow::Result<f64> {
        Ok(this.get().elapsed(this.now))
    }
    /// Seconds since the last output line, heartbeat or structured event.
    fn silence(this: &SRun) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this.get().silence(this.now)))
    }
    /// Seconds set by `::expect silence=…`, or None for the default.
    fn expect_silence(this: &SRun) -> anyhow::Result<NoneOr<f64>> {
        Ok(opt(this
            .get()
            .expect_silence_ms
            .map(|ms| ms as f64 / 1000.0)))
    }
    /// Output lines per minute over the window (default 60 s).
    fn lines_per_min<'v>(this: &SRun, window: Option<Value<'v>>) -> anyhow::Result<f64> {
        let w = self::window(window)?.unwrap_or(60.0).max(1.0);
        Ok(this.get().output.count(this.now, Some(w)) as f64 * 60.0 / w)
    }
    /// A metric's series (empty if the run never reported it).
    fn metric(this: &SRun, name: &str) -> anyhow::Result<SSeries> {
        Ok(this.series(this.get().metrics.get(name).unwrap_or(&EMPTY)))
    }
    /// Names of all metrics the run has reported, sorted.
    fn metrics(this: &SRun) -> anyhow::Result<Vec<String>> {
        let mut v: Vec<String> = this.get().metrics.keys().cloned().collect();
        v.sort();
        Ok(v)
    }
    /// A step's `current` over time (recorded when it changes). Without an id: the
    /// most recently begun running step that reports progress.
    fn progress<'v>(this: &SRun, id: Option<Value<'v>>) -> anyhow::Result<SSeries> {
        let d = this.get();
        let id: Option<String> = match id {
            Some(v) if !v.is_none() => Some(
                v.unpack_str()
                    .ok_or_else(|| anyhow::anyhow!("step id must be a string"))?
                    .to_string(),
            ),
            _ => d
                .steps
                .iter()
                .rev()
                .find(|s| s.running && d.progress.contains_key(&s.id))
                .map(|s| s.id.clone()),
        };
        let s = id
            .as_deref()
            .and_then(|i| d.progress.get(i))
            .unwrap_or(&EMPTY);
        Ok(this.series(s))
    }
    /// Steps as dicts: id, name, parent, running, current, total, unit, started (seconds ago).
    fn steps<'v>(this: &SRun, heap: Heap<'v>) -> anyhow::Result<Value<'v>> {
        let now = this.now;
        let items: Vec<Value<'v>> = this
            .get()
            .steps
            .iter()
            .map(|s| {
                let none = Value::new_none();
                let f = |x: Option<f64>| x.map(|v| heap.alloc(v)).unwrap_or(none);
                heap.alloc(AllocDict([
                    ("id", heap.alloc(s.id.as_str())),
                    ("name", heap.alloc(s.name.as_str())),
                    (
                        "parent",
                        s.parent.as_deref().map(|p| heap.alloc(p)).unwrap_or(none),
                    ),
                    ("running", Value::new_bool(s.running)),
                    ("current", f(s.current)),
                    ("total", f(s.total)),
                    (
                        "unit",
                        s.unit.as_deref().map(|u| heap.alloc(u)).unwrap_or(none),
                    ),
                    ("age", heap.alloc((now - s.started_at) as f64 / 1000.0)),
                ]))
            })
            .collect();
        Ok(heap.alloc(AllocList(items)))
    }
    /// CPU % of the run's process tree (100 = one core).
    fn proc_cpu(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().proc_cpu))
    }
    /// Resident memory of the run's process tree, bytes.
    fn proc_mem(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().proc_mem))
    }
    /// Host CPU %, all cores.
    fn host_cpu(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().host_cpu))
    }
    /// Host memory used, %.
    fn host_mem(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().host_mem))
    }
    /// Free bytes on the volume holding the run's working directory.
    fn disk_free(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().disk_free))
    }
    /// GPU utilization % of the GPUs the run uses (empty until GPU sampling exists).
    fn gpu_util(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().gpu_util))
    }
    fn gpu_mem(this: &SRun) -> anyhow::Result<SSeries> {
        Ok(this.series(&this.get().gpu_mem))
    }
    /// A threshold from `[defaults.checks]` in config.toml (durations in seconds).
    fn config<'v>(this: &SRun, key: &str, default: Value<'v>) -> anyhow::Result<f64> {
        if let Some(v) = this.get().config.get(key) {
            return Ok(*v);
        }
        to_f64(default)
            .ok_or_else(|| anyhow::anyhow!("config default for '{key}' must be a number"))
    }
}
starlark::methods_static!(RUN_METHODS = run_methods);

fn num_arg(v: Value) -> anyhow::Result<f64> {
    to_f64(v).ok_or_else(|| anyhow::anyhow!("expected a number, got {}", v.get_type()))
}

fn key_arg<'v>(key: Option<Value<'v>>) -> anyhow::Result<Option<String>> {
    match key {
        Some(v) if !v.is_none() => Ok(Some(
            v.unpack_str()
                .ok_or_else(|| anyhow::anyhow!("key must be a string"))?
                .to_string(),
        )),
        _ => Ok(None),
    }
}

#[starlark_module]
fn check_globals(builder: &mut GlobalsBuilder) {
    fn secs<'v>(n: Value<'v>) -> anyhow::Result<f64> {
        num_arg(n)
    }
    fn mins<'v>(n: Value<'v>) -> anyhow::Result<f64> {
        Ok(num_arg(n)? * 60.0)
    }
    fn hours<'v>(n: Value<'v>) -> anyhow::Result<f64> {
        Ok(num_arg(n)? * 3600.0)
    }
    /// `is_nan(None)` is False, so `is_nan(series.last())` is safe on empty series.
    fn is_nan<'v>(x: Value<'v>) -> anyhow::Result<bool> {
        Ok(to_f64(x).is_some_and(f64::is_nan))
    }
    fn is_inf<'v>(x: Value<'v>) -> anyhow::Result<bool> {
        Ok(to_f64(x).is_some_and(f64::is_infinite))
    }
    fn is_finite<'v>(x: Value<'v>) -> anyhow::Result<bool> {
        Ok(to_f64(x).is_some_and(f64::is_finite))
    }
    /// Seconds → "4m12s".
    fn fmt_duration<'v>(s: Value<'v>) -> anyhow::Result<String> {
        Ok(fmt_secs(num_arg(s)?))
    }
    fn info<'v>(message: &str, key: Option<Value<'v>>) -> anyhow::Result<NoneType> {
        record(ActionKind::Info, message, key_arg(key)?.as_deref());
        Ok(NoneType)
    }
    fn warn<'v>(message: &str, key: Option<Value<'v>>) -> anyhow::Result<NoneType> {
        record(ActionKind::Warn, message, key_arg(key)?.as_deref());
        Ok(NoneType)
    }
    fn stalled<'v>(message: &str, key: Option<Value<'v>>) -> anyhow::Result<NoneType> {
        record(ActionKind::Stalled, message, key_arg(key)?.as_deref());
        Ok(NoneType)
    }
    fn fail<'v>(message: &str, key: Option<Value<'v>>) -> anyhow::Result<NoneType> {
        record(ActionKind::Fail, message, key_arg(key)?.as_deref());
        Ok(NoneType)
    }
    fn kill<'v>(message: &str, key: Option<Value<'v>>) -> anyhow::Result<NoneType> {
        record(ActionKind::Kill, message, key_arg(key)?.as_deref());
        Ok(NoneType)
    }
    fn notify<'v>(message: &str, key: Option<Value<'v>>) -> anyhow::Result<NoneType> {
        record(ActionKind::Notify, message, key_arg(key)?.as_deref());
        Ok(NoneType)
    }
}

pub fn fmt_secs(s: f64) -> String {
    if !s.is_finite() {
        return format!("{s}");
    }
    let s = s.max(0.0);
    if s < 60.0 {
        return format!("{s:.0}s");
    }
    let t = s as u64;
    let (h, m, sec) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m{sec:02}s")
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct Engine {
    globals: Globals,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            globals: GlobalsBuilder::standard().with(check_globals).build(),
        }
    }

    /// Parse and run the top level (META, helpers). Errors are rustc-style text.
    pub fn compile(&self, src: &CheckSource) -> Result<Compiled, String> {
        let ast = AstModule::parse(&src.origin, src.text.clone(), &Dialect::Standard)
            .map_err(|e| e.to_string())?;
        let module = Module::with_temp_heap(|m| {
            {
                let mut eval = Evaluator::new(&m);
                let deadline = Instant::now() + EVAL_BUDGET;
                eval.set_check_cancelled(Box::new(move || Instant::now() > deadline));
                eval.eval_module(ast, &self.globals)
                    .map_err(|e| e.to_string())?;
            }
            m.freeze().map_err(|e| format!("{e:?}"))
        })?;
        if module.get("check").is_err() {
            return Err(format!(
                "{}: a check file must define `def check(run):`",
                src.origin
            ));
        }
        let meta = read_meta(&module).map_err(|e| format!("{}: META: {e}", src.origin))?;
        Ok(Compiled {
            name: src.name.clone(),
            origin: src.origin.clone(),
            meta,
            module,
        })
    }

    /// Call `check(run)` once. Returns the actions it took.
    pub fn evaluate(
        &self,
        c: &Compiled,
        data: &RunData,
        now: Millis,
    ) -> Result<Vec<Action>, String> {
        ACTIONS.with(|a| a.borrow_mut().clear());
        let result = Module::with_temp_heap(|m| -> Result<(), String> {
            let f = c.module.get("check").map_err(|e| e.to_string())?;
            // SAFETY: `c.module` outlives this call, keeping the frozen value alive.
            let f = unsafe { f.unchecked_frozen_value() }.to_value();
            let mut eval = Evaluator::new(&m);
            let deadline = Instant::now() + EVAL_BUDGET;
            eval.set_check_cancelled(Box::new(move || Instant::now() > deadline));
            let run = m.heap().alloc(SRun {
                ptr: data as *const RunData,
                now,
            });
            eval.eval_function(f, &[run], &[]).map(|_| ()).map_err(|e| {
                if Instant::now() > deadline {
                    format!(
                        "check exceeded its {} ms time budget",
                        EVAL_BUDGET.as_millis()
                    )
                } else {
                    e.to_string()
                }
            })
        });
        let actions = ACTIONS.with(|a| std::mem::take(&mut *a.borrow_mut()));
        result.map(|_| actions)
    }
}

fn read_meta(module: &FrozenModule) -> Result<Meta, String> {
    let mut meta = Meta::default();
    let Ok(v) = module.get("META") else {
        return Ok(meta);
    };
    let v = v.value();
    let d = DictRef::from_value(v.to_value()).ok_or("META must be a dict")?;
    for (k, val) in d.iter() {
        let key = k.unpack_str().ok_or("META keys must be strings")?;
        match key {
            "applies" => {
                meta.applies = Some(
                    val.unpack_str()
                        .ok_or("applies must be a string glob")?
                        .to_string(),
                )
            }
            "every" => meta.every = to_f64(val).ok_or("every must be seconds")?.max(1.0),
            "grace" => meta.grace = to_f64(val).ok_or("grace must be seconds")?.max(0.0),
            "kill" => meta.kill = val.unpack_bool().ok_or("kill must be True/False")?,
            "clear_after" => {
                meta.clear_after =
                    val.unpack_i32()
                        .filter(|n| *n >= 1)
                        .ok_or("clear_after must be an int >= 1")? as u32
            }
            other => {
                return Err(format!(
                    "unknown key '{other}' (expected applies, every, grace, kill, clear_after)"
                ));
            }
        }
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use trun_proto::{Event, EventKind, Lifecycle, Num};

    fn src(text: &str) -> CheckSource {
        CheckSource {
            name: "t".into(),
            origin: "checks/t.star".into(),
            text: text.into(),
        }
    }

    fn data() -> RunData {
        let mut d = RunData::new("R1", "train-x", "p");
        let ev = |seq, ts, kind| Event {
            run_id: "R1".into(),
            seq,
            ts,
            kind,
        };
        d.ingest(&ev(
            1,
            0,
            EventKind::Lifecycle {
                state: Lifecycle::Running,
                exit_code: None,
                signal: None,
                pid: None,
                reason: None,
            },
        ));
        for i in 0..10 {
            let v = if i == 9 {
                f64::NAN
            } else {
                1.0 - i as f64 * 0.01
            };
            d.ingest(&ev(
                2 + i,
                i as i64 * 1000,
                EventKind::Metric {
                    values: BTreeMap::from([("loss".to_string(), Num(v))]),
                    step: Some(i as i64),
                },
            ));
        }
        d
    }

    #[test]
    fn evaluates_and_collects_actions() {
        let e = Engine::new();
        let c = e
            .compile(&src(r#"
META = {"applies": "train*", "every": secs(5), "grace": mins(1), "kill": True}
def check(run):
    loss = run.metric("loss")
    if is_nan(loss.last()):
        fail("loss went NaN at %s" % run.name)
    if loss.slope(mins(1)) < 0:
        info("improving", key="trend")
    if run.metric("nope").avg() == None and not run.metric("nope").exists():
        warn("no nope")
"#))
            .unwrap();
        assert_eq!(c.meta.applies.as_deref(), Some("train*"));
        assert_eq!(c.meta.every, 5.0);
        assert_eq!(c.meta.grace, 60.0);
        assert!(c.meta.kill);
        let actions = e.evaluate(&c, &data(), 9_000).unwrap();
        let kinds: Vec<_> = actions
            .iter()
            .map(|a| (a.kind, a.message.as_str(), a.key.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (ActionKind::Fail, "loss went NaN at train-x", None),
                (ActionKind::Info, "improving", Some("trend")),
                (ActionKind::Warn, "no nope", None),
            ]
        );
    }

    #[test]
    fn helpful_errors() {
        let e = Engine::new();
        let err = e
            .compile(&src("def check(run):\n    x = [1, 2\n"))
            .unwrap_err();
        assert!(err.contains("checks/t.star"), "{err}");
        let err = e.compile(&src("X = 1\n")).unwrap_err();
        assert!(err.contains("def check(run)"), "{err}");
        let err = e
            .compile(&src("META = {\"evry\": 5}\ndef check(run):\n    pass\n"))
            .unwrap_err();
        assert!(err.contains("unknown key 'evry'"), "{err}");
        let c = e
            .compile(&src("def check(run):\n    run.metric('loss').avg() + 1\n"))
            .unwrap();
        let err = e.evaluate(&c, &RunData::default(), 0).unwrap_err();
        assert!(err.contains("NoneType") || err.contains("None"), "{err}");
    }

    #[test]
    fn runaway_loop_is_cancelled() {
        let e = Engine::new();
        let c = e.compile(&src("def check(run):\n    for a in range(100000000):\n        for b in range(100000000):\n            pass\n")).unwrap();
        let t0 = Instant::now();
        let err = e.evaluate(&c, &RunData::default(), 0).unwrap_err();
        assert!(err.contains("time budget"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    }

    #[test]
    fn steps_and_progress_api() {
        let e = Engine::new();
        let mut d = RunData::new("R", "n", "p");
        let ev = |seq, ts, kind| Event {
            run_id: "R".into(),
            seq,
            ts,
            kind,
        };
        d.ingest(&ev(
            1,
            0,
            EventKind::StepBegin {
                id: "train".into(),
                name: "Train".into(),
                parent: None,
            },
        ));
        d.ingest(&ev(
            2,
            1000,
            EventKind::Progress {
                id: "train".into(),
                current: 3.0,
                total: Some(10.0),
                unit: None,
                rate: None,
                eta_ms: None,
            },
        ));
        let c = e
            .compile(&src(r#"
def check(run):
    for s in run.steps():
        if s["running"] and run.progress(s["id"]).age() > 60:
            stalled("step %s stuck at %d/%d" % (s["name"], s["current"], s["total"]))
    if run.progress().last() != 3:
        fail("default progress should be the running step")
"#))
            .unwrap();
        let a = e.evaluate(&c, &d, 120_000).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].message, "step Train stuck at 3/10");
    }
}

//! Panel and dashboard files (docs/06-panels.md).
//!
//! Loaded from a run's project (`<config_root>/.trun/{panels,dashboards}`) and the
//! global `$TRUN_HOME/{panels,dashboards}`; project files override global ones by
//! name. Every file is validated strictly (unknown keys are errors) and errors are
//! returned per file, so a human or an AI agent can fix a panel from the message
//! alone while the rest of the dashboard keeps working.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    Line,
    Area,
    Bar,
    Scatter,
    Stat,
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XAxis {
    #[default]
    Step,
    Time,
    Elapsed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scale {
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    #[serde(default)]
    pub below: Option<f64>,
    #[serde(default)]
    pub above: Option<f64>,
    pub color: String,
}

fn default_source() -> String {
    "metric".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelSpec {
    #[serde(default)]
    pub title: Option<String>,
    pub kind: PanelKind,
    #[serde(default = "default_source")]
    pub source: String,
    pub y: OneOrMany,
    #[serde(default)]
    pub x: XAxis,
    /// EMA smoothing factor in [0, 1) (display only).
    #[serde(default)]
    pub smooth: Option<f64>,
    #[serde(default)]
    pub scale: Option<Scale>,
    /// Which runs to plot. Only `current` exists until run comparison lands.
    #[serde(default)]
    pub runs: Option<String>,
    /// Stat tiles: `last` (default), `min`, `max`, `mean`, or `avg:<duration>`.
    #[serde(default)]
    pub reduce: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub thresholds: Vec<Threshold>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub panel: String,
    #[serde(default)]
    pub span: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub panels: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardSpec {
    #[serde(default)]
    pub title: Option<String>,
    /// Glob on the run name: this dashboard is the default for matching runs.
    #[serde(default)]
    pub applies: Option<String>,
    #[serde(default)]
    pub columns: Option<u32>,
    #[serde(default)]
    pub row: Vec<Row>,
}

pub const BUILTINS: &[&str] = &[
    "builtin:header",
    "builtin:diagnosis",
    "builtin:steps",
    "builtin:metrics",
    "builtin:log",
];

#[derive(Debug, Clone, Serialize)]
pub struct Loaded<T> {
    pub name: String,
    pub file: String,
    /// `project` or `global`.
    pub scope: &'static str,
    pub spec: Option<T>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PanelSet {
    pub panels: Vec<Loaded<PanelSpec>>,
    pub dashboards: Vec<Loaded<DashboardSpec>>,
    /// Dashboard chosen for this run (first whose `applies` matches its name).
    pub dashboard: Option<String>,
    /// Directories searched, most specific first.
    pub dirs: Vec<String>,
}

fn validate_panel(p: &PanelSpec) -> Result<(), String> {
    if p.source != "metric" {
        return Err(format!(
            "source '{}' is not supported yet (only 'metric')",
            p.source
        ));
    }
    let ys = p.y.to_vec();
    if ys.is_empty() || ys.iter().any(|y| y.trim().is_empty()) {
        return Err("y must name at least one metric".into());
    }
    if let Some(s) = p.smooth
        && !(0.0..1.0).contains(&s)
    {
        return Err(format!("smooth must be in [0, 1), got {s}"));
    }
    if let Some(sc) = &p.scale
        && let Some(y) = &sc.y
        && !matches!(y.as_str(), "linear" | "log" | "symlog")
    {
        return Err(format!("scale.y must be linear, log or symlog, got '{y}'"));
    }
    if let Some(r) = &p.runs
        && r != "current"
    {
        return Err(format!(
            "runs = '{r}' is not supported yet (only 'current')"
        ));
    }
    if let Some(r) = &p.reduce {
        let ok = matches!(r.as_str(), "last" | "min" | "max" | "mean")
            || r.strip_prefix("avg:")
                .is_some_and(|d| trun_parse::parse_duration_ms(d).is_some());
        if !ok {
            return Err(format!(
                "reduce must be last, min, max, mean or avg:<duration>, got '{r}'"
            ));
        }
    }
    for t in &p.thresholds {
        if t.below.is_none() && t.above.is_none() {
            return Err("each threshold needs `below` or `above`".into());
        }
    }
    Ok(())
}

fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn load_panels(dir: &Path, scope: &'static str, out: &mut BTreeMap<String, Loaded<PanelSpec>>) {
    for path in read_dir_sorted(dir) {
        let name = stem(&path);
        if name.starts_with('.') || out.contains_key(&name) {
            continue;
        }
        let file = path.to_string_lossy().into_owned();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let loaded = match ext {
            "toml" => match std::fs::read_to_string(&path) {
                Ok(text) => match toml::from_str::<PanelSpec>(&text) {
                    Ok(spec) => match validate_panel(&spec) {
                        Ok(()) => Loaded {
                            name: name.clone(),
                            file,
                            scope,
                            spec: Some(spec),
                            error: None,
                        },
                        Err(e) => Loaded {
                            name: name.clone(),
                            file,
                            scope,
                            spec: None,
                            error: Some(e),
                        },
                    },
                    Err(e) => Loaded {
                        name: name.clone(),
                        file,
                        scope,
                        spec: None,
                        error: Some(e.to_string()),
                    },
                },
                Err(e) => Loaded {
                    name: name.clone(),
                    file,
                    scope,
                    spec: None,
                    error: Some(e.to_string()),
                },
            },
            "ts" | "tsx" | "js" => Loaded {
                name: name.clone(),
                file,
                scope,
                spec: None,
                error: Some(
                    "TypeScript panels are not supported yet (planned: M6); use a TOML panel"
                        .into(),
                ),
            },
            _ => continue,
        };
        out.insert(name, loaded);
    }
}

fn load_dashboards(
    dir: &Path,
    scope: &'static str,
    out: &mut BTreeMap<String, Loaded<DashboardSpec>>,
) {
    for path in read_dir_sorted(dir) {
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = stem(&path);
        if name.starts_with('.') || out.contains_key(&name) {
            continue;
        }
        let file = path.to_string_lossy().into_owned();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str::<DashboardSpec>(&t).map_err(|e| e.to_string()));
        let loaded = match parsed {
            Ok(spec) => Loaded {
                name: name.clone(),
                file,
                scope,
                spec: Some(spec),
                error: None,
            },
            Err(e) => Loaded {
                name: name.clone(),
                file,
                scope,
                spec: None,
                error: Some(e),
            },
        };
        out.insert(name, loaded);
    }
}

/// Load everything visible to a run.
pub fn load(config_root: Option<&Path>, home: &Path, run_name: &str) -> PanelSet {
    let mut dirs: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Some(root) = config_root {
        dirs.push((root.join(".trun"), "project"));
    }
    dirs.push((home.to_path_buf(), "global"));

    let mut panels = BTreeMap::new();
    let mut dashboards = BTreeMap::new();
    for (d, scope) in &dirs {
        load_panels(&d.join("panels"), scope, &mut panels);
        load_dashboards(&d.join("dashboards"), scope, &mut dashboards);
    }
    // Dashboards referencing unknown panels get an error (the rest still renders).
    for db in dashboards.values_mut() {
        if let Some(spec) = &db.spec {
            let missing: Vec<&str> = spec
                .row
                .iter()
                .flat_map(|r| r.panels.iter())
                .map(|c| c.panel.as_str())
                .filter(|p| !p.starts_with("builtin:") && !panels.contains_key(*p))
                .collect();
            let bad_builtin: Vec<&str> = spec
                .row
                .iter()
                .flat_map(|r| r.panels.iter())
                .map(|c| c.panel.as_str())
                .filter(|p| p.starts_with("builtin:") && !BUILTINS.contains(p))
                .collect();
            if !missing.is_empty() {
                db.error = Some(format!("unknown panel(s): {}", missing.join(", ")));
            } else if !bad_builtin.is_empty() {
                db.error = Some(format!(
                    "unknown builtin(s): {} (available: {})",
                    bad_builtin.join(", "),
                    BUILTINS.join(", ")
                ));
            }
        }
    }
    let dashboard = dashboards
        .values()
        .find(|d| {
            d.spec
                .as_ref()
                .and_then(|s| s.applies.as_deref())
                .is_some_and(|g| glob_match(g, run_name))
        })
        .map(|d| d.name.clone());
    PanelSet {
        panels: panels.into_values().collect(),
        dashboards: dashboards.into_values().collect(),
        dashboard,
        dirs: dirs
            .iter()
            .map(|(d, _)| d.to_string_lossy().into_owned())
            .collect(),
    }
}

/// `*` and `?` glob.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trun-panels-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn globs() {
        assert!(glob_match("train*", "train-v3"));
        assert!(glob_match("*", "x"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("train*", "eval"));
    }

    #[test]
    fn loads_validates_and_overrides() {
        let root = tmp("proj");
        let home = tmp("home");
        std::fs::create_dir_all(root.join(".trun/panels")).unwrap();
        std::fs::create_dir_all(root.join(".trun/dashboards")).unwrap();
        std::fs::create_dir_all(home.join("panels")).unwrap();
        std::fs::write(root.join(".trun/panels/loss.toml"), "title = \"Loss\"\nkind = \"line\"\ny = [\"loss\", \"val_loss\"]\nsmooth = 0.9\nscale.y = \"log\"\n").unwrap();
        std::fs::write(
            root.join(".trun/panels/typo.toml"),
            "kind = \"line\"\ny = \"loss\"\ncolour = \"red\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".trun/panels/bad.toml"),
            "kind = \"line\"\ny = \"loss\"\nsmooth = 2\n",
        )
        .unwrap();
        std::fs::write(home.join("panels/loss.toml"), "kind = \"bar\"\ny = \"x\"\n").unwrap();
        std::fs::write(
            home.join("panels/tp.toml"),
            "kind = \"stat\"\ny = \"throughput\"\nreduce = \"avg:1m\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".trun/dashboards/training.toml"),
            "applies = \"train*\"\n[[row]]\npanels = [{ panel = \"loss\", span = 8 }, { panel = \"builtin:log\" }]\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".trun/dashboards/broken.toml"),
            "[[row]]\npanels = [{ panel = \"nope\" }]\n",
        )
        .unwrap();

        let set = load(Some(&root), &home, "train-v3");
        let by = |n: &str| set.panels.iter().find(|p| p.name == n).unwrap();
        assert_eq!(by("loss").scope, "project", "project overrides global");
        assert_eq!(by("loss").spec.as_ref().unwrap().kind, PanelKind::Line);
        assert!(
            by("typo").error.as_ref().unwrap().contains("colour"),
            "{:?}",
            by("typo").error
        );
        assert!(by("bad").error.as_ref().unwrap().contains("smooth"));
        assert!(by("tp").error.is_none());
        assert_eq!(set.dashboard.as_deref(), Some("training"));
        let broken = set.dashboards.iter().find(|d| d.name == "broken").unwrap();
        assert!(broken.error.as_ref().unwrap().contains("nope"));

        assert_eq!(load(Some(&root), &home, "eval").dashboard, None);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&home);
    }
}

//! `$TRUN_HOME/config.toml` (docs/09-project-layout.md).
//!
//! ```toml
//! [defaults.checks]      # thresholds for the built-in checks; durations or numbers
//! silence = "5m"
//! no_progress = "15m"
//! disk_warn_bytes = 2e9
//!
//! [notify]
//! desktop = true         # native notifications for stalls, failures, notify()
//! ```

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Seconds for durations, raw numbers otherwise.
    pub check_thresholds: BTreeMap<String, f64>,
    pub desktop_notifications: bool,
    /// Problems found while reading the file (reported, not fatal).
    pub warnings: Vec<String>,
}

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    defaults: RawDefaults,
    #[serde(default)]
    notify: RawNotify,
}

#[derive(Deserialize, Default)]
struct RawDefaults {
    #[serde(default)]
    checks: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct RawNotify {
    #[serde(default = "yes")]
    desktop: bool,
}

impl Default for RawNotify {
    fn default() -> Self {
        RawNotify { desktop: true }
    }
}

fn yes() -> bool {
    true
}

impl Config {
    pub fn load(home: &Path) -> Config {
        let path = home.join("config.toml");
        let mut cfg = Config {
            desktop_notifications: true,
            ..Default::default()
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return cfg;
        };
        let raw: Raw = match toml::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                cfg.warnings.push(format!("{}: {e}", path.display()));
                return cfg;
            }
        };
        cfg.desktop_notifications = raw.notify.desktop;
        for (k, v) in raw.defaults.checks {
            let parsed = match &v {
                toml::Value::Integer(i) => Some(*i as f64),
                toml::Value::Float(f) => Some(*f),
                toml::Value::String(s) => {
                    trun_parse::parse_duration_ms(s).map(|ms| ms as f64 / 1000.0)
                }
                _ => None,
            };
            match parsed {
                Some(x) => {
                    cfg.check_thresholds.insert(k, x);
                }
                None => cfg.warnings.push(format!(
                    "{}: [defaults.checks] {k} = {v} is not a number or duration",
                    path.display()
                )),
            }
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_thresholds_and_notify() {
        let dir = std::env::temp_dir().join(format!("trun-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[defaults.checks]\nsilence = \"90s\"\ndisk_warn_bytes = 5e9\nzombie = 600\nbad = true\n[notify]\ndesktop = false\n",
        )
        .unwrap();
        let c = Config::load(&dir);
        assert_eq!(c.check_thresholds["silence"], 90.0);
        assert_eq!(c.check_thresholds["disk_warn_bytes"], 5e9);
        assert_eq!(c.check_thresholds["zombie"], 600.0);
        assert!(!c.desktop_notifications);
        assert_eq!(c.warnings.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(Config::load(Path::new("/definitely/missing")).desktop_notifications);
    }
}

//! Hot reload: watch `.trun/{panels,dashboards}` directories and tell subscribers
//! when anything in them changes (docs/06-panels.md).
//!
//! One OS watcher serves every project. A project root is watched non-recursively
//! (to notice `.trun/` being created) and its `.trun/` recursively once it exists.
//! Changes are debounced, then every subscriber of the affected root gets a new
//! version number and reloads.

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

const DEBOUNCE: Duration = Duration::from_millis(80);

#[derive(Clone)]
pub struct ConfigWatcher {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    watcher: Option<RecommendedWatcher>,
    /// Project root → (whether `.trun/` is watched, change notifications).
    roots: HashMap<PathBuf, (bool, broadcast::Sender<u64>)>,
    /// The global config dir (`$TRUN_HOME`); changes there affect every run.
    global: PathBuf,
    global_tx: broadcast::Sender<u64>,
    version: u64,
}

fn relevant(rel: &Path) -> bool {
    rel.components().next().is_some_and(|c| {
        let c = c.as_os_str();
        c == "panels" || c == "dashboards"
    })
}

impl ConfigWatcher {
    pub fn new(global: PathBuf) -> Self {
        let (raw_tx, raw_rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = raw_tx.send(res);
        });
        let watcher = match watcher {
            Ok(mut w) => {
                for sub in ["panels", "dashboards"] {
                    let d = global.join(sub);
                    let _ = std::fs::create_dir_all(&d);
                    if let Err(e) = w.watch(&d, RecursiveMode::Recursive) {
                        tracing::warn!(dir = %d.display(), error = %e, "cannot watch global config dir");
                    }
                }
                Some(w)
            }
            Err(e) => {
                tracing::warn!(error = %e, "file watching unavailable; panels will not hot-reload");
                None
            }
        };
        let (global_tx, _) = broadcast::channel(16);
        let cw = ConfigWatcher {
            inner: Arc::new(Mutex::new(Inner {
                watcher,
                roots: HashMap::new(),
                global,
                global_tx,
                version: 0,
            })),
        };
        let me = cw.clone();
        tokio::spawn(async move { me.run(raw_rx).await });
        cw
    }

    /// Changes relevant to runs whose config lives under `root` (plus global changes).
    pub fn subscribe(
        &self,
        root: Option<&Path>,
    ) -> (broadcast::Receiver<u64>, broadcast::Receiver<u64>) {
        let mut g = self.inner.lock().unwrap();
        let global_rx = g.global_tx.subscribe();
        let Some(root) = root else {
            let (tx, rx) = broadcast::channel(1);
            drop(tx);
            return (rx, global_rx);
        };
        let root = root.to_path_buf();
        if !g.roots.contains_key(&root) {
            let (tx, _) = broadcast::channel(16);
            let trun = root.join(".trun");
            let mut armed = false;
            if let Some(w) = g.watcher.as_mut() {
                let _ = w.watch(&root, RecursiveMode::NonRecursive);
                armed = trun.is_dir() && w.watch(&trun, RecursiveMode::Recursive).is_ok();
            }
            g.roots.insert(root.clone(), (armed, tx));
        }
        (g.roots[&root].1.subscribe(), global_rx)
    }

    async fn run(self, mut raw: mpsc::UnboundedReceiver<notify::Result<notify::Event>>) {
        while let Some(first) = raw.recv().await {
            let mut batch = vec![first];
            let deadline = tokio::time::Instant::now() + DEBOUNCE;
            while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, raw.recv()).await {
                batch.push(ev);
            }
            self.handle(batch);
        }
    }

    fn handle(&self, batch: Vec<notify::Result<notify::Event>>) {
        let mut g = self.inner.lock().unwrap();
        let mut touched_roots: Vec<PathBuf> = Vec::new();
        let mut global = false;
        let mut to_arm: Vec<PathBuf> = Vec::new();
        for ev in batch.into_iter().flatten() {
            if matches!(ev.kind, EventKind::Access(_)) {
                continue;
            }
            for path in &ev.paths {
                if let Ok(rel) = path.strip_prefix(&g.global)
                    && relevant(rel)
                {
                    global = true;
                }
                for (root, (armed, _)) in &g.roots {
                    let trun = root.join(".trun");
                    if path == &trun {
                        if !armed {
                            to_arm.push(root.clone());
                        }
                        touched_roots.push(root.clone());
                    } else if let Ok(rel) = path.strip_prefix(&trun)
                        && (relevant(rel) || rel.as_os_str().is_empty())
                    {
                        touched_roots.push(root.clone());
                    }
                }
            }
        }
        for root in to_arm {
            let trun = root.join(".trun");
            let ok = g
                .watcher
                .as_mut()
                .is_some_and(|w| w.watch(&trun, RecursiveMode::Recursive).is_ok());
            if let Some(e) = g.roots.get_mut(&root) {
                e.0 = ok;
            }
        }
        if global || !touched_roots.is_empty() {
            g.version += 1;
            let v = g.version;
            if global {
                let _ = g.global_tx.send(v);
            }
            touched_roots.sort();
            touched_roots.dedup();
            for r in touched_roots {
                if let Some((_, tx)) = g.roots.get(&r) {
                    let _ = tx.send(v);
                }
            }
        }
    }
}

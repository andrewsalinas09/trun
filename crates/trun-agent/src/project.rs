//! Which project does a run belong to? (docs/02-architecture.md, "What is a project?")
//!
//! 1. explicit `--project`
//! 2. `name` in the nearest `.trun/config.toml` walking up from cwd
//! 3. the git repository name (from the `origin` remote URL, else the root dir name)
//! 4. `_adhoc`
//!
//! Names come from config or git rather than local paths, so the same repository on
//! two machines maps to the same project.

use std::path::{Path, PathBuf};
use trun_proto::ADHOC_PROJECT;

pub fn resolve_project(explicit: Option<&str>, cwd: &Path) -> String {
    if let Some(p) = explicit.map(str::trim).filter(|p| !p.is_empty()) {
        return p.to_string();
    }
    // ~/.trun holds the *global* config, which never names a project.
    let home = dirs::home_dir();
    for dir in cwd.ancestors() {
        if home.as_deref() == Some(dir) {
            continue;
        }
        if let Some(name) = trun_config_name(dir) {
            return name;
        }
        if let Some(name) = git_project_name(dir) {
            return name;
        }
    }
    ADHOC_PROJECT.to_string()
}

fn trun_config_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".trun").join("config.toml")).ok()?;
    let value: toml::Table = toml::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// If `dir` is a git work tree root, derive the project name.
fn git_project_name(dir: &Path) -> Option<String> {
    let dotgit = dir.join(".git");
    let git_dir = if dotgit.is_dir() {
        dotgit
    } else if dotgit.is_file() {
        // Worktree / submodule: `gitdir: <path>`
        let text = std::fs::read_to_string(&dotgit).ok()?;
        let rel = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let p = PathBuf::from(rel);
        let p = if p.is_absolute() { p } else { dir.join(p) };
        common_git_dir(&p)
    } else {
        return None;
    };
    let from_remote = std::fs::read_to_string(git_dir.join("config"))
        .ok()
        .and_then(|c| origin_repo_name(&c));
    from_remote.or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// For a worktree gitdir (`.git/worktrees/<name>`), the shared config lives in the
/// common dir named by the `commondir` file.
fn common_git_dir(gitdir: &Path) -> PathBuf {
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(rel) => gitdir.join(rel.trim()),
        Err(_) => gitdir.to_path_buf(),
    }
}

/// Parse `[remote "origin"] url = ...` from a git config and return the repo name.
pub fn origin_repo_name(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.replace(' ', "") == "[remote\"origin\"]";
            continue;
        }
        if in_origin
            && let Some(rest) = line.strip_prefix("url")
            && let Some(url) = rest.trim_start().strip_prefix('=')
        {
            return repo_name_from_url(url.trim());
        }
    }
    None
}

fn repo_name_from_url(url: &str) -> Option<String> {
    let last = url.trim_end_matches('/').rsplit(['/', ':', '\\']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names_from_urls() {
        assert_eq!(
            repo_name_from_url("git@github.com:andrewsalinas09/trun.git").as_deref(),
            Some("trun")
        );
        assert_eq!(
            repo_name_from_url("https://github.com/a/b-c/").as_deref(),
            Some("b-c")
        );
        assert_eq!(
            repo_name_from_url("/srv/git/detector.git").as_deref(),
            Some("detector")
        );
    }

    #[test]
    fn parses_origin_from_config() {
        let cfg = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = x/other.git\n[remote \"origin\"]\n\turl = git@github.com:me/detector.git\n";
        assert_eq!(origin_repo_name(cfg).as_deref(), Some("detector"));
    }

    #[test]
    fn resolution_order() {
        let root = std::env::temp_dir().join(format!("trun-proj-{}", std::process::id()));
        let sub = root.join("repo").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join("repo").join(".git")).unwrap();
        std::fs::write(
            root.join("repo/.git/config"),
            "[remote \"origin\"]\n url = https://x/y/fromgit.git\n",
        )
        .unwrap();

        assert_eq!(resolve_project(Some("explicit"), &sub), "explicit");
        assert_eq!(resolve_project(None, &sub), "fromgit");

        std::fs::create_dir_all(root.join("repo/.trun")).unwrap();
        std::fs::write(
            root.join("repo/.trun/config.toml"),
            "name = \"fromconfig\"\n",
        )
        .unwrap();
        assert_eq!(resolve_project(None, &sub), "fromconfig");

        let _ = std::fs::remove_dir_all(&root);
    }
}

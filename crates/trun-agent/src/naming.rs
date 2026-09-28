//! Default run names derived from the command line.

use std::path::Path;

/// Interpreters: the script is the interesting part (`python train.py` → `train.py`).
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "bash",
    "sh",
    "zsh",
    "pwsh",
    "powershell",
];

/// Tools whose subcommand is the interesting part (`cargo test` → `cargo test`).
const SUBCOMMAND_TOOLS: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "make", "go", "uv", "poetry", "just", "npx", "dotnet", "git",
    "docker", "pytest",
];

pub fn derive_name(cmd: &[String]) -> String {
    let Some(program) = cmd.first() else {
        return "run".into();
    };
    let stem = Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| program.clone());
    let first_arg = cmd[1..].iter().find(|a| !a.starts_with('-'));

    if INTERPRETERS.contains(&stem.as_str()) {
        if let Some(a) = first_arg {
            return Path::new(a)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| a.clone());
        }
    } else if SUBCOMMAND_TOOLS.contains(&stem.as_str())
        && let Some(a) = first_arg
    {
        return format!("{stem} {a}");
    }
    stem
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(parts: &[&str]) -> String {
        derive_name(&parts.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn names() {
        assert_eq!(
            n(&["python", "-u", "scripts/train.py", "--epochs", "5"]),
            "train.py"
        );
        assert_eq!(n(&["cargo", "test", "--release"]), "cargo test");
        assert_eq!(n(&["C:\\bin\\ffmpeg.exe", "-i", "x"]), "ffmpeg");
        assert_eq!(n(&["python"]), "python");
    }
}

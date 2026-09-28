//! Locations under the trun home directory (`$TRUN_HOME`, default `~/.trun`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

/// Written by a running hub so clients can find it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    pub pid: u32,
    pub port: u16,
    pub url: String,
    pub started_at: i64,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let home = match std::env::var_os("TRUN_HOME") {
            Some(h) => PathBuf::from(h),
            None => dirs::home_dir()
                .context("cannot determine home directory")?
                .join(".trun"),
        };
        Ok(Paths { home })
    }

    pub fn data_dir(&self) -> PathBuf {
        self.home.join("data")
    }

    pub fn token_file(&self) -> PathBuf {
        self.home.join("hub.token")
    }

    pub fn hub_info_file(&self) -> PathBuf {
        self.home.join("hub.json")
    }

    pub fn log_file(&self) -> PathBuf {
        self.home.join("hub.log")
    }

    /// The per-user API token, created on first use. Only this user can read it,
    /// which is what keeps other local users out of the loopback API.
    pub fn load_or_create_token(&self) -> Result<String> {
        let path = self.token_file();
        if let Ok(t) = std::fs::read_to_string(&path) {
            let t = t.trim().to_string();
            if t.len() >= 32 {
                return Ok(t);
            }
        }
        std::fs::create_dir_all(&self.home)?;
        use rand::Rng;
        let bytes: [u8; 32] = rand::rng().random();
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        write_private(&path, &token)?;
        Ok(token)
    }

    pub fn read_hub_info(&self) -> Option<HubInfo> {
        let text = std::fs::read_to_string(self.hub_info_file()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write_hub_info(&self, info: &HubInfo) -> Result<()> {
        std::fs::create_dir_all(&self.home)?;
        std::fs::write(self.hub_info_file(), serde_json::to_string_pretty(info)?)?;
        Ok(())
    }
}

fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        // The user profile directory is already private to the user on Windows.
        std::fs::write(path, contents)?;
    }
    Ok(())
}

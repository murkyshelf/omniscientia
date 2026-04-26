use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const CONFIG_FILE: &str = "config.json";
pub const ENV_FILE:    &str = ".env";
/// Agent workspace — files the agent can read/write to freely
pub const WORKSPACE:   &str = "workspace";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct AppConfig {
    pub ollama_base: String,
    pub model_name: String,
}

#[derive(Debug, Default, Clone)]
pub struct Secrets {
    pub telegram_token: String,
    pub discord_token:  String,
}

impl AppConfig {
    pub fn load() -> Option<Self> {
        serde_json::from_str(&fs::read_to_string(CONFIG_FILE).ok()?).ok()
    }

    /// Write to config.json (no workspace creation — handled in main).
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(CONFIG_FILE, content)?;
        Ok(())
    }
}

impl Secrets {
    pub fn load() -> Self {
        let _ = dotenvy::from_filename(ENV_FILE);
        Secrets {
            telegram_token: std::env::var("TELEGRAM_TOKEN").unwrap_or_default(),
            discord_token:  std::env::var("DISCORD_TOKEN").unwrap_or_default(),
        }
    }

    pub fn save(&self) -> Result<(), std::io::Error> {
        let mut lines = Vec::new();
        if !self.telegram_token.is_empty() {
            lines.push(format!("TELEGRAM_TOKEN={}", self.telegram_token));
        }
        if !self.discord_token.is_empty() {
            lines.push(format!("DISCORD_TOKEN={}", self.discord_token));
        }
        fs::write(ENV_FILE, lines.join("\n"))
    }
}

/// Remove obsolete `.omniscientia` file left by previous versions.
pub fn migrate_old_config() {
    let old = Path::new(".omniscientia");
    if old.exists() && old.is_file() {
        let _ = fs::remove_file(old);
    }
}

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Remembered between runs. Never holds a password or a token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The server address as the user typed it.
    pub server: String,
    /// True when the user ticked "allow unencrypted connection".
    pub insecure: bool,
    pub username: String,
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("screentalk").join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best effort: a read-only config directory must not break the app.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| std::fs::write(&path, serde_json::to_vec_pretty(self).unwrap_or_default()));
        if let Err(e) = result {
            tracing::warn!("could not save settings to {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let c: Config = serde_json::from_str(r#"{"server":"example.com"}"#).unwrap();
        assert_eq!(c.server, "example.com");
        assert!(!c.insecure);
        assert_eq!(c.username, "");
    }
}

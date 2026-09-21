//! Persistance des réglages de l'application (JSON dans le répertoire de configuration).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::BuildOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Dernier dossier de destination choisi
    pub dest_dir: Option<String>,
    /// Chemins des dossiers de drivers à intégrer (copiés vers Drivers/ALL,
    /// appliqués aux images Windows ET WinPE/WinRE par le convertisseur officiel)
    pub drivers: Vec<String>,
    /// Canal sélectionné (canary, dev, beta, rp, retail)
    pub channel: String,
    /// Architecture sélectionnée (amd64, arm64, x86)
    pub arch: String,
    /// Options de création
    pub options: BuildOptions,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dest_dir: None,
            drivers: Vec::new(),
            channel: "dev".into(),
            arch: "amd64".into(),
            options: BuildOptions::default(),
        }
    }
}

impl Settings {
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("uupdump-client").join("settings.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::config_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip() {
        let mut s = Settings::default();
        s.drivers.push("C:\\Drivers\\test".into());
        let j = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&j).unwrap();
        assert_eq!(s2.channel, "dev");
        assert_eq!(s2.drivers.len(), 1);
        assert!(s2.options.make_iso);
    }
}

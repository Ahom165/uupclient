//! Persistance de la configuration utilisateur (JSON dans le dossier de config du système).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverTarget {
    /// Injecté dans toutes les images (install.wim toutes éditions).
    All,
    /// Injecté uniquement dans l'image OS (install.wim).
    Os,
    /// Injecté dans boot.wim / WinPE (setup).
    WinPe,
}

impl DriverTarget {
    pub fn folder(&self) -> &'static str {
        match self {
            DriverTarget::All => "ALL",
            DriverTarget::Os => "OS",
            DriverTarget::WinPe => "WinPE",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            DriverTarget::All => "Toutes images",
            DriverTarget::Os => "OS (install.wim)",
            DriverTarget::WinPe => "WinPE (boot.wim)",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverEntry {
    pub path: String,
    pub target: DriverTarget,
}

/// Format de compression de l'image d'installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compression {
    /// install.wim (standard DISM /Compress:max)
    Wim,
    /// install.esd (compression recovery, plus petit)
    Esd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageOptions {
    /// Convertir en ISO à la fin du téléchargement (sinon : fichiers UUP seuls).
    pub convert_iso: bool,
    /// Format install.wim / install.esd
    pub compression: Compression,
    /// Intégrer les mises à jour cumulatives au media (AddUpdates).
    pub add_updates: bool,
    /// Nettoyage des composants obsolètes après intégration (Cleanup) — "auto-clean".
    pub cleanup: bool,
    /// ResetBase : réduit encore la taille, empêche la désinstallation des mises à jour.
    pub reset_base: bool,
    /// Intégrer .NET Framework 3.5 (NetFx3).
    pub netfx3: bool,
    /// Créer les éditions virtuelles (Pro for Workstations, Education, ...).
    pub virtual_editions: bool,
    /// Supprimer Microsoft Edge du media (SkipEdge).
    pub skip_edge: bool,
    /// Ne pas recréer winre.wim (SkipWinRE).
    pub skip_winre: bool,
    /// Nettoyage automatique des fichiers UUP/temp après création de l'ISO (côté appli).
    pub auto_clean: bool,
}

impl Default for PackageOptions {
    fn default() -> Self {
        Self {
            convert_iso: true,
            compression: Compression::Wim,
            add_updates: true,
            cleanup: true,
            reset_base: false,
            netfx3: false,
            virtual_editions: false,
            skip_edge: false,
            skip_winre: false,
            auto_clean: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub search_text: String,
    pub channel: String,
    pub arch: String,
    pub output_dir: String,
    pub options: PackageOptions,
    pub drivers: Vec<DriverEntry>,
    pub last_lang: String,
}

impl Default for Config {
    fn default() -> Self {
        let out = default_output_dir();
        Self {
            search_text: String::new(),
            channel: "retail".into(),
            arch: "amd64".into(),
            output_dir: out,
            options: PackageOptions::default(),
            drivers: Vec::new(),
            last_lang: "fr-fr".into(),
        }
    }
}

pub fn default_output_dir() -> String {
    if let Some(home) = dirs::home_dir() {
        return home.join("UUPs").to_string_lossy().into_owned();
    }
    ".".into()
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("uupdump-client")
        .join("config.json")
}

pub fn load() -> Config {
    let path = config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
            return cfg;
        }
    }
    Config::default()
}

pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(&path, json);
    }
}

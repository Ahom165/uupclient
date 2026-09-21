//! Types de données partagés : builds, fichiers UUP, configuration d'un projet de téléchargement.

use serde::{Deserialize, Serialize};

/// Une build Windows renvoyée par listid.php / fetchupd.php
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Build {
    pub title: String,
    pub build: String,
    pub arch: String,
    pub created: i64,
    pub uuid: String,
}

/// Entrée fichier renvoyée par get.php
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub expire: i64,
}

/// Langue disponible pour une build
#[derive(Debug, Clone)]
pub struct LangEntry {
    pub code: String,
    pub fancy: String,
}

/// Édition disponible pour une build + langue
#[derive(Debug, Clone)]
pub struct EditionEntry {
    pub key: String,
    pub fancy: String,
}

/// Mode de téléchargement
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DlMode {
    /// Set complet (langue + éditions) prêt pour conversion ISO
    Full,
    /// Uniquement les fichiers de mise à jour cumulatives (UPDATEONLY)
    UpdatesOnly,
}

/// Paramètres de création (patchés dans ConvertConfig.ini officiel)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOptions {
    /// Convertir les fichiers UUP en ISO (sinon: fichiers seuls)
    pub make_iso: bool,
    /// Intégrer les mises à jour cumulatives dans l'image (AddUpdates)
    pub add_updates: bool,
    /// Le convertisseur supprime ses fichiers temporaires (Cleanup)
    pub cleanup: bool,
    /// Réinitialise la base de composants après intégration (ResetBase)
    pub reset_base: bool,
    /// Ne pas réintégrer Edge (SkipEdge)
    pub skip_edge: bool,
    /// Compression ESD au lieu de WIM
    pub esd: bool,
    /// Créer les éditions virtuelles (Enterprise, etc. depuis Pro)
    pub virtual_editions: bool,
    /// Vérifier les empreintes SHA-1 après téléchargement
    pub verify_hashes: bool,
    /// Télécharger aussi les applications du Microsoft Store
    pub store_apps: bool,
    /// Nombre de téléchargements en parallèle
    pub threads: usize,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            make_iso: true,
            add_updates: true,
            cleanup: true,
            reset_base: false,
            skip_edge: false,
            esd: false,
            virtual_editions: false,
            verify_hashes: true,
            store_apps: false,
            threads: 4,
        }
    }
}

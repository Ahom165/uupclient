//! Client pour l'API JSON publique d'UUP dump (https://api.uupdump.net).
//!
//! Endpoints utilisés (validés en production) :
//!   - listid.php       : recherche de builds (mots-clés / tri par date)
//!   - fetchupd.php     : dernière build d'un canal (canary, dev, beta, rp, retail)
//!   - listlangs.php    : langues disponibles pour une build
//!   - listeditions.php : éditions disponibles pour une build + langue
//!   - get.php          : liste des fichiers avec URLs de téléchargement signées
//!
//! Toutes les réponses sont enveloppées : {"response": {...}, "jsonApiVersion": "..."}.
//! Une erreur applicative donne {"response": {"error": "CODE"}} avec HTTP 400/500.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::models::{Build, EditionEntry, FileEntry, LangEntry};

pub const API_BASE: &str = "https://api.uupdump.net";

/// Canaux proposés dans l'interface.
pub const CHANNELS: &[(&str, &str)] = &[
    ("canary", "Canary"),
    ("dev", "Dev"),
    ("beta", "Beta"),
    ("rp", "Release Preview"),
    ("retail", "Retail"),
];

/// Architectures proposées dans l'interface.
pub const ARCHS: &[&str] = &["amd64", "arm64", "x86"];

/// Erreur haute niveau avec message utilisateur en français.
#[derive(Debug)]
pub struct ApiError {
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ApiError {}

fn err(msg: impl Into<String>) -> ApiError {
    ApiError { message: msg.into() }
}

/// Traduit un code d'erreur UUP dump en message français (utilisé aussi par le builder).
pub fn api_translate(code: &str) -> String {
    translate_error(code)
}

/// Traduit un code d'erreur de l'API en message français compréhensible.
fn translate_error(code: &str) -> String {
    match code {
        "UNSUPPORTED_COMBINATION" => {
            "Cette build n'est pas (encore) disponible avec cette combinaison langue/édition côté serveur. Réessaie plus tard ou choisis une autre build.".into()
        }
        "UNSUPPORTED_LANG" => "Langue non supportée pour cette build.".into(),
        "INCORRECT_ID" => "Identifiant d'update invalide.".into(),
        "UNSPECIFIED_UPDATE" => "Aucune update spécifiée.".into(),
        "MISSING_FILES" => "Des fichiers manquent côté serveur Windows Update.".into(),
        "NOT_CUMULATIVE_UPDATE" => "Cette update n'est pas une mise à jour cumulative.".into(),
        "EMPTY_FILELIST" => "Liste de fichiers vide côté Windows Update.".into(),
        "WU_REQUEST_FAILED" => "La requête vers les serveurs Windows Update a échoué.".into(),
        "USER_RATE_LIMITED" => "Trop de requêtes vers UUP dump, patiente une minute avant de réessayer.".into(),
        other => {
            let _ = other;
            format!("Erreur UUP dump : {}", code)
        }
    }
}

/// Client HTTP avec retry automatique sur les erreurs temporaires (429/5xx/réseau).
#[derive(Clone)]
pub struct ApiClient {
    agent: ureq::Agent,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiClient {
    pub fn new() -> Self {
        let mut agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .user_agent("uupdump-client-rs/0.1 (client natif)")
            .build();
        // gzip déjà actif via la feature "gzip" de ureq ; agent utilisé pour toutes les requêtes
        let _ = &mut agent;
        Self { agent }
    }

    /// GET JSON avec retry (jusqu'à 4 tentatives, backoff croissant).
    fn get_json(&self, url: &str) -> Result<Value, ApiError> {
        let mut last_err = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(2u64.pow(attempt as u32).min(30)));
            }
            let resp = self
                .agent
                .get(url)
                .call();
            match resp {
                Ok(r) => {
                    let mut body = String::new();
                    r.into_reader()
                        .take(64 * 1024 * 1024)
                        .read_to_string(&mut body)
                        .map_err(|e| err(format!("Lecture de la réponse : {e}")))?;
                    let v: Value = serde_json::from_str(&body)
                        .map_err(|e| err(format!("Réponse illisible ({e})")))?;
                    return Ok(v);
                }
                Err(ureq::Error::Status(code, r)) => {
                    // L'API renvoie 400/500 avec un corps JSON contenant l'erreur : on le lit quand même.
                    let mut body = String::new();
                    let _ = r.into_reader().take(4 * 1024 * 1024).read_to_string(&mut body);
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        if let Some(e) = v.pointer("/response/error").and_then(|x| x.as_str()) {
                            return Err(err(translate_error(e)));
                        }
                    }
                    last_err = format!("HTTP {code}");
                    if code == 429 || code >= 500 {
                        continue; // temporaire -> retry
                    }
                    return Err(err(format!("Le serveur a répondu {code}")));
                }
                Err(e) => {
                    last_err = format!("{e}");
                    continue; // réseau -> retry
                }
            }
        }
        Err(err(format!("Requête impossible après plusieurs essais ({last_err})")))
    }

    /// Extrait la liste des builds d'une réponse listid/fetchupd.
    /// Le champ "builds" peut être un tableau (fetchupd) ou un objet indexé (listid/search).
    fn parse_builds(v: &Value) -> Vec<Build> {
        let mut out = Vec::new();
        let Some(builds) = v.pointer("/response/builds") else {
            return out;
        };
        match builds {
            Value::Array(arr) => {
                for b in arr {
                    if let Some(build) = parse_one_build(b) {
                        out.push(build);
                    }
                }
            }
            Value::Object(map) => {
                for (_, b) in map {
                    if let Some(build) = parse_one_build(b) {
                        out.push(build);
                    }
                }
            }
            _ => {}
        }
        out.sort_by(|a, b| b.created.cmp(&a.created).then(a.title.cmp(&b.title)));
        out
    }

    /// Recherche de builds par mot-clé (n° de build, "insider", "server"...).
    pub fn search(&self, query: &str) -> Result<Vec<Build>, ApiError> {
        let q: String = query.trim().replace(' ', "+");
        let url = if q.is_empty() {
            format!("{API_BASE}/listid.php?sortByDate=1")
        } else {
            format!("{API_BASE}/listid.php?search={q}&sortByDate=1")
        };
        let v = self.get_json(&url)?;
        Ok(Self::parse_builds(&v))
    }

    /// Récupère la (les) dernière(s) build d'un canal pour une architecture.
    pub fn fetch_channel(&self, arch: &str, ring: &str) -> Result<Vec<Build>, ApiError> {
        let url = format!("{API_BASE}/fetchupd.php?arch={arch}&ring={ring}");
        let v = self.get_json(&url)?;
        Ok(Self::parse_builds(&v))
    }

    /// Langues disponibles pour une build.
    pub fn list_langs(&self, update_id: &str) -> Result<Vec<LangEntry>, ApiError> {
        let url = format!("{API_BASE}/listlangs.php?id={update_id}");
        let v = self.get_json(&url)?;
        let resp = &v["response"];
        if let Some(e) = resp.get("error").and_then(|x| x.as_str()) {
            return Err(err(translate_error(e)));
        }
        let mut out = Vec::new();
        let fancy: BTreeMap<String, String> = resp
            .get("langFancyNames")
            .and_then(|x| x.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(list) = resp.get("langList").and_then(|x| x.as_array()) {
            for code in list {
                if let Some(code) = code.as_str() {
                    let label = fancy
                        .get(code)
                        .cloned()
                        .unwrap_or_else(|| prettify_lang(code));
                    out.push(LangEntry {
                        code: code.to_string(),
                        fancy: label,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.fancy.to_lowercase().cmp(&b.fancy.to_lowercase()));
        Ok(out)
    }

    /// Éditions disponibles pour une build + langue.
    pub fn list_editions(&self, update_id: &str, lang: &str) -> Result<Vec<EditionEntry>, ApiError> {
        let url = format!("{API_BASE}/listeditions.php?id={update_id}&lang={lang}");
        let v = self.get_json(&url)?;
        let resp = &v["response"];
        if let Some(e) = resp.get("error").and_then(|x| x.as_str()) {
            return Err(err(translate_error(e)));
        }
        let mut out = Vec::new();
        let fancy: BTreeMap<String, String> = resp
            .get("editionFancyNames")
            .and_then(|x| x.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(list) = resp.get("editionList").and_then(|x| x.as_array()) {
            for ed in list {
                if let Some(key) = ed.as_str() {
                    let label = fancy
                        .get(key)
                        .cloned()
                        .unwrap_or_else(|| key.to_string());
                    out.push(EditionEntry {
                        key: key.to_string(),
                        fancy: label,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.fancy.to_lowercase().cmp(&b.fancy.to_lowercase()));
        Ok(out)
    }

    /// Liste des fichiers pour une build + langue + éditions (fusion de plusieurs appels).
    /// `edition = None` avec `lang = None` => UPDATEONLY (mises à jour seules).
    pub fn get_files(
        &self,
        update_id: &str,
        lang: Option<&str>,
        editions: &[String],
    ) -> Result<(String, Vec<FileEntry>), ApiError> {
        let mut merged: BTreeMap<String, FileEntry> = BTreeMap::new();
        let mut update_name = String::new();

        let targets: Vec<(Option<&str>, Option<&str>)> = if lang.is_none() {
            vec![(None, Some("UPDATEONLY"))]
        } else if editions.is_empty() {
            vec![(lang, None)]
        } else {
            editions.iter().map(|e| (lang, Some(e.as_str()))).collect()
        };

        for (lang_p, ed_p) in targets {
            let mut url = format!("{API_BASE}/get.php?id={update_id}");
            if let Some(l) = lang_p {
                url.push_str(&format!("&lang={l}"));
            }
            if let Some(e) = ed_p {
                url.push_str(&format!("&edition={e}"));
            }
            let v = self.get_json(&url)?;
            let resp = &v["response"];
            if let Some(e) = resp.get("error").and_then(|x| x.as_str()) {
                return Err(err(translate_error(e)));
            }
            if update_name.is_empty() {
                update_name = resp
                    .get("updateName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("Update")
                    .to_string();
            }
            if let Some(files) = resp.get("files").and_then(|x| x.as_object()) {
                for (name, meta) in files {
                    let entry = FileEntry {
                        name: name.clone(),
                        sha1: meta.get("sha1").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        sha256: meta.get("sha256").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        size: meta
                            .get("size")
                            .and_then(|x| x.as_str().map(|s| s.to_string()).or_else(|| x.as_i64().map(|i| i.to_string())))
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0),
                        url: meta.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        expire: meta.get("expire").and_then(|x| x.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| x.as_i64())).unwrap_or(0),
                    };
                    // Fusion : dédoublonnage par nom (les éditions partagent la majorité des fichiers)
                    merged.entry(entry.name.clone()).or_insert(entry);
                }
            }
        }

        let files: Vec<FileEntry> = merged.into_values().collect();
        if files.is_empty() {
            return Err(err("Aucun fichier retourné pour cette sélection."));
        }
        Ok((update_name, files))
    }
}

fn parse_one_build(v: &Value) -> Option<Build> {
    Some(Build {
        title: v.get("title")?.as_str()?.to_string(),
        build: v.get("build")?.as_str()?.to_string(),
        arch: v.get("arch")?.as_str()?.to_string(),
        created: v.get("created")?.as_i64()?,
        uuid: v.get("uuid")?.as_str()?.to_string(),
    })
}

/// "fr-fr" -> "Français (France)" pour les codes les plus courants (fallback si l'API
/// ne fournit pas de nom fantaisiste).
fn prettify_lang(code: &str) -> String {
    let map: &[(&str, &str)] = &[
        ("fr-fr", "Français (France)"),
        ("fr-ca", "Français (Canada)"),
        ("en-us", "Anglais (États-Unis)"),
        ("en-gb", "Anglais (Royaume-Uni)"),
        ("de-de", "Allemand"),
        ("es-es", "Espagnol (Espagne)"),
        ("es-mx", "Espagnol (Mexique)"),
        ("it-it", "Italien"),
        ("pt-br", "Portugais (Brésil)"),
        ("pt-pt", "Portugais (Portugal)"),
        ("nl-nl", "Néerlandais"),
        ("ru-ru", "Russe"),
        ("pl-pl", "Polonais"),
        ("zh-cn", "Chinois simplifié"),
        ("zh-tw", "Chinois traditionnel"),
        ("ja-jp", "Japonais"),
        ("ko-kr", "Coréen"),
        ("ar-sa", "Arabe"),
        ("he-il", "Hébreu"),
        ("neutral", "Neutre"),
    ];
    map.iter()
        .find(|(k, _)| *k == code.to_lowercase())
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_builds_object_shape() {
        let v: Value = serde_json::json!({
            "response": {"builds": {"1": {"title":"T","build":"1.0","arch":"amd64","created":10,"uuid":"u"}}}
        });
        let b = ApiClient::parse_builds(&v);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].uuid, "u");
        assert_eq!(b[0].arch, "amd64");
    }

    #[test]
    fn parse_builds_array_shape() {
        let v: Value = serde_json::json!({
            "response": {"builds": [{"title":"T2","build":"2.0","arch":"arm64","created":20,"uuid":"u2"}]}
        });
        let b = ApiClient::parse_builds(&v);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].build, "2.0");
    }

    #[test]
    fn lang_prettify() {
        assert_eq!(prettify_lang("fr-fr"), "Français (France)");
        assert_eq!(prettify_lang("neutral"), "Neutre");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Tests réseau réels : cargo test -- --ignored
    #[test]
    #[ignore]
    fn live_search_and_files() {
        let api = ApiClient::new();
        let builds = api.search("Windows 11 Insider Preview").expect("recherche");
        assert!(!builds.is_empty(), "aucune build insider");
        // Trouve une build qui expose des langues (les KB n'en ont pas).
        let mut chosen: Option<(Build, Vec<LangEntry>)> = None;
        for b in builds.iter().take(6) {
            if let Ok(langs) = api.list_langs(&b.uuid) {
                if !langs.is_empty() {
                    chosen = Some((b.clone(), langs));
                    break;
                }
            }
        }
        let Some((b, langs)) = chosen else {
            panic!("aucune build avec langues trouvée");
        };
        assert!(!b.uuid.is_empty());
        let fr = langs
            .iter()
            .find(|l| l.code == "fr-fr")
            .cloned()
            .or_else(|| langs.first().cloned())
            .expect("aucune langue");
        let eds = api.list_editions(&b.uuid, &fr.code).expect("éditions");
        if eds.is_empty() {
            eprintln!("(pas d'éditions pour cette build — ok pour un KB)");
            return;
        }
        let (name, files) = api
            .get_files(&b.uuid, Some(&fr.code), &[eds[0].key.clone()])
            .expect("fichiers");
        assert!(!files.is_empty());
        assert!(files.iter().all(|f| !f.url.is_empty()));
        eprintln!("OK : {name} → {} fichiers", files.len());
    }
}

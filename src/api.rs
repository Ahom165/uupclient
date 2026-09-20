//! Client pour l'API JSON de UUP dump (api.uupdump.net).
//!
//! Endpoints validés contre l'API réelle (voir research/ dans le dépôt du projet) :
//! - fetchupd.php      : arch, ring, build, flight, sku   -> dernières builds depuis Windows Update
//! - listid.php        : search, sortByDate              -> base de données des builds connues (recherche du site)
//! - listlangs.php     : id                              -> langues disponibles pour une update
//! - listeditions.php  : lang, id                        -> éditions disponibles pour une langue
//! - get.php           : id, lang, edition, noLinks      -> liste de fichiers + URLs directes + SHA-256
//!
//! Toutes les réponses sont enveloppées dans { "response": ..., "jsonApiVersion": ... }.
//! Erreurs notables : USER_RATE_LIMITED (HTTP 429), NO_UPDATE_FOUND, UNSUPPORTED_COMBINATION.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

pub const API_BASE: &str = "https://api.uupdump.net";
/// Package du convertisseur officiel (Windows) hébergé par uupdump.net.
pub const CONVERTER_7Z_URL: &str = "https://uupdump.net/misc/uup-converter-wimlib-v126.7z";
pub const CONVERTER_7Z_SHA256: &str =
    "1448f7e33353fa63d558333c4b6ddb8b41b992a7911f19d3b72ba3246cf349bc";
/// Extraction 7z sous Windows (fourni par uupdump).
pub const SEVEN_ZR_URL: &str = "https://uupdump.net/misc/7zr.exe";
pub const SEVEN_ZR_SHA256: &str =
    "72c98287b2e8f85ea7bb87834b6ce1ce7ce7f41a8c97a81b307d4d4bf900922b";
/// aria2c.exe pour Windows (accélère fortement les téléchargements).
pub const ARIA2C_URL: &str = "https://uupdump.net/misc/aria2c.exe";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEntry {
    pub title: String,
    pub build: String,
    pub arch: String,
    pub created: i64,
    pub uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCandidate {
    pub update_id: String,
    pub title: String,
    pub build: String,
    pub arch: String,
}

#[derive(Debug, Clone)]
pub struct Langs {
    pub list: Vec<String>,
    pub fancy: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Editions {
    pub list: Vec<String>,
    pub fancy: HashMap<String, String>,
}

/// Désérialisation tolérante : l'API renvoie parfois les nombres sous forme de chaînes.
fn de_i64_flexible<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        I(i64),
        F(f64),
        S(String),
    }
    match V::deserialize(d) {
        Ok(V::I(i)) => Ok(i),
        Ok(V::F(f)) => Ok(f as i64),
        Ok(V::S(s)) => s.trim().parse::<i64>().map_err(serde::de::Error::custom),
        Err(e) => Err(e),
    }
}

/// Désérialise une map nom→texte en acceptant `[]` (tableau vide) : l'API
/// renvoie un tableau vide au lieu d'un objet quand il n'y a aucune entrée
/// (ex. langFancyNames pour une update sans langues).
fn de_map_flexible<'de, D>(d: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Object(m) => Ok(m
            .into_iter()
            .filter_map(|(k, x)| x.as_str().map(|s| (k, s.to_string())))
            .collect()),
        // L'API renvoie [] (tableau vide) au lieu d'un objet quand il n'y a
        // aucune entrée (ex. langFancyNames d'une update sans langues).
        _ => Ok(HashMap::new()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default, deserialize_with = "de_i64_flexible")]
    pub size: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub uuid: String,
    #[serde(default, deserialize_with = "de_i64_flexible")]
    pub expire: i64,
}

#[derive(Debug, Clone)]
pub struct PackageFiles {
    pub update_name: String,
    pub arch: String,
    pub build: String,
    pub files: HashMap<String, FileInfo>,
}

#[derive(Debug)]
pub enum ApiError {
    /// HTTP 429 : trop de requêtes, réessayer dans quelques secondes.
    RateLimited,
    /// Erreur renvoyée par l'API (NO_UPDATE_FOUND, UNSUPPORTED_COMBINATION, ...).
    Api(String),
    /// Problème réseau / HTTP.
    Http(String),
    /// Réponse inattendue.
    Parse(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::RateLimited => write!(
                f,
                "Trop de requêtes vers UUP dump (limite de débit). Réessayez dans quelques secondes."
            ),
            ApiError::Api(e) => {
                if let Some(msg) = friendly_api_error(e) {
                    write!(f, "{msg}")
                } else {
                    write!(f, "Erreur API UUP dump : {e}")
                }
            }
            ApiError::Http(e) => write!(f, "Erreur réseau : {e}"),
            ApiError::Parse(e) => write!(f, "Réponse illisible : {e}"),
        }
    }
}

/// Traduction des codes d'erreur connus de l'API en messages utilisables.
fn friendly_api_error(code: &str) -> Option<&'static str> {
    match code {
        "NO_UPDATE_FOUND" => Some(
            "Aucune build trouvée pour cette combinaison canal / arch / numéro. En canal Retail, indiquez un numéro précis (ex. 19045 ou 26100) ; « latest » ne s'applique qu'aux canaux Insider (Dev, Beta, Canary).",
        ),
        "UNSUPPORTED_COMBINATION" => {
            Some("Combinaison canal / arch / build non supportée par Windows Update.")
        }
        "ILLEGAL_BUILD" => Some("Numéro de build invalide (minimum 6256)."),
        "UNKNOWN_RING" => Some("Canal inconnu."),
        "UNKNOWN_ARCH" => Some("Architecture inconnue."),
        "NOT_FOUND" | "UPDATE_NOT_FOUND" => Some("Identifiant d'update introuvable sur UUP dump."),
        _ => None,
    }
}

/// Délai avant nouvelle tentative après un 429 (l'API limite à ~1 requête/10 s
/// pour une ressource différente de la précédente).
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(6);

fn get_json<T: for<'de> Deserialize<'de>>(path: &str, query: &str) -> Result<T, ApiError> {
    // L'API limite le débit par IP : on réessaie automatiquement quelques fois.
    let mut attempt = 0;
    loop {
        attempt += 1;
        match get_json_once(path, query) {
            Err(ApiError::RateLimited) if attempt < 4 => {
                std::thread::sleep(RATE_LIMIT_BACKOFF);
            }
            other => return other,
        }
    }
}

fn get_json_once<T: for<'de> Deserialize<'de>>(path: &str, query: &str) -> Result<T, ApiError> {
    let agent: ureq::Agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .user_agent("uupdump-client-rs/0.1")
        .build();

    let url = format!("{API_BASE}/{path}?{query}");
    let text = match agent.get(&url).call() {
        Ok(r) => r.into_string().map_err(|e| ApiError::Http(e.to_string()))?,
        Err(ureq::Error::Status(429, _)) => return Err(ApiError::RateLimited),
        Err(ureq::Error::Status(code, resp)) => {
            // L'API renvoie souvent un JSON exploitable avec un code 4xx/5xx
            // ({"response":{"error":"NO_UPDATE_FOUND"}}). On parse en Value
            // brut : une struct T dont tous les champs sont default
            // désérialiserait ce corps en Data et masquerait l'erreur.
            let body = resp.into_string().unwrap_or_default();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(e) = extract_error(&v) {
                    return Err(e);
                }
            }
            return Err(ApiError::Http(format!("HTTP {code}")));
        }
        Err(e) => return Err(ApiError::Http(e.to_string())),
    };

    // Chemin nominal : détecter aussi une erreur embarquée dans un 200.
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| ApiError::Parse(e.to_string()))?;
    if let Some(e) = extract_error(&v) {
        return Err(e);
    }
    let data_v = v
        .get("response")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value(data_v).map_err(|e| ApiError::Parse(e.to_string()))
}

/// Extrait l'erreur API d'une enveloppe {"response":{"error": …}} si présente.
fn extract_error(v: &serde_json::Value) -> Option<ApiError> {
    let err = v.get("response")?.get("error")?.as_str()?.to_string();
    Some(if err == "USER_RATE_LIMITED" {
        ApiError::RateLimited
    } else {
        ApiError::Api(err)
    })
}

/// Recherche dans la base des builds connues — c'est la recherche "Tout parcourir" du site.
///
/// NOTE : avec `search`, l'API renvoie `builds` sous forme d'objet indexé
/// ({"builds": {"18": {...}}}) ; sans recherche, c'est un tableau. On accepte les deux.
pub fn list_ids(search: &str, sort_by_date: bool) -> Result<Vec<BuildEntry>, ApiError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BuildsField {
        Array(Vec<serde_json::Value>),
        Map(BTreeMap<String, serde_json::Value>),
    }
    #[derive(Deserialize)]
    struct Resp {
        builds: BuildsField,
    }

    let mut query = String::new();
    if !search.is_empty() {
        query.push_str("search=");
        query.push_str(&percent_encode(search));
        query.push('&');
    }
    query.push_str(&format!("sortByDate={}", if sort_by_date { 1 } else { 0 }));

    let r: Resp = get_json("listid.php", &query)?;
    // Décodage entrée par entrée : une entrée malformée est ignorée au lieu de faire
    // échouer toute la réponse (certaines entrées historiques de la base sont douteuses).
    let raw: Vec<serde_json::Value> = match r.builds {
        BuildsField::Array(v) => v,
        BuildsField::Map(m) => m.into_values().collect(),
    };
    let mut out: Vec<BuildEntry> = raw
        .into_iter()
        .filter_map(|v| serde_json::from_value::<BuildEntry>(v).ok())
        .collect();
    if sort_by_date {
        out.sort_by(|a, b| b.created.cmp(&a.created));
    }
    Ok(out)
}

/// Récupère les dernières builds directement depuis Windows Update.
/// `ring` : canary | dev | beta | rp | retail  —  `build` : "latest" | "26100" | "26100.1742"
pub fn fetch_upd(arch: &str, ring: &str, build: &str) -> Result<Vec<UpdateCandidate>, ApiError> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "updateId", default)]
        update_id: String,
        #[serde(rename = "updateTitle", default)]
        update_title: String,
        #[serde(rename = "foundBuild", default)]
        found_build: String,
        #[serde(default)]
        arch: String,
        #[serde(rename = "updateArray", default)]
        update_array: Vec<RawUpdate>,
    }
    #[derive(Deserialize)]
    struct RawUpdate {
        #[serde(rename = "updateId")]
        update_id: String,
        #[serde(rename = "updateTitle")]
        update_title: String,
        #[serde(rename = "foundBuild")]
        found_build: String,
        #[serde(default)]
        arch: String,
    }

    let query = format!(
        "arch={}&ring={}&build={}&flight=Active&sku=48",
        percent_encode(arch),
        percent_encode(ring),
        percent_encode(build)
    );
    let r: Resp = get_json("fetchupd.php", &query)?;

    let mut out: Vec<UpdateCandidate> = r
        .update_array
        .into_iter()
        .map(|u| UpdateCandidate {
            update_id: u.update_id,
            title: u.update_title,
            build: u.found_build,
            arch: if u.arch.is_empty() {
                arch.to_string()
            } else {
                u.arch
            },
        })
        .collect();
    if out.is_empty() && !r.update_id.is_empty() {
        out.push(UpdateCandidate {
            update_id: r.update_id,
            title: r.update_title,
            build: r.found_build,
            arch: if r.arch.is_empty() {
                arch.to_string()
            } else {
                r.arch
            },
        });
    }
    Ok(out)
}

/// Langues disponibles pour une update.
pub fn list_langs(id: &str) -> Result<Langs, ApiError> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "langList", default)]
        lang_list: Vec<String>,
        #[serde(
            rename = "langFancyNames",
            default,
            deserialize_with = "de_map_flexible"
        )]
        lang_fancy: HashMap<String, String>,
    }
    let r: Resp = get_json("listlangs.php", &format!("id={}", percent_encode(id)))?;
    Ok(Langs {
        list: r.lang_list,
        fancy: r.lang_fancy,
    })
}

/// Éditions disponibles pour une update + langue.
pub fn list_editions(lang: &str, id: &str) -> Result<Editions, ApiError> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "editionList", default)]
        edition_list: Vec<String>,
        #[serde(
            rename = "editionFancyNames",
            default,
            deserialize_with = "de_map_flexible"
        )]
        edition_fancy: HashMap<String, String>,
    }
    let query = format!("lang={}&id={}", percent_encode(lang), percent_encode(id));
    let r: Resp = get_json("listeditions.php", &query)?;
    Ok(Editions {
        list: r.edition_list,
        fancy: r.edition_fancy,
    })
}

/// Liste des fichiers d'un package. `edition` = nom d'édition ("PROFESSIONAL") ou "0" (toutes).
pub fn get_files(id: &str, lang: &str, edition: &str) -> Result<PackageFiles, ApiError> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "updateName", default)]
        update_name: String,
        #[serde(default)]
        arch: String,
        #[serde(default)]
        build: String,
        #[serde(default)]
        files: BTreeMap<String, serde_json::Value>,
    }
    let query = format!(
        "id={}&lang={}&edition={}",
        percent_encode(id),
        percent_encode(lang),
        percent_encode(edition)
    );
    let r: Resp = get_json("get.php", &query)?;
    // Décodage tolérant fichier par fichier (les entrées défectueuses sont ignorées).
    let files: HashMap<String, FileInfo> = r
        .files
        .into_iter()
        .filter_map(|(k, v)| serde_json::from_value::<FileInfo>(v).ok().map(|fi| (k, fi)))
        .collect();
    Ok(PackageFiles {
        update_name: r.update_name,
        arch: r.arch,
        build: r.build,
        files,
    })
}

/// Encodage minimaliste pour query string (les UUID/titres ne contiennent pas de caractères exotiques,
/// mais on encode proprement au cas où).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'+' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test réseau réel : lancer avec `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn listid_search_works() {
        let r = list_ids("26100", true).expect("API call");
        assert!(!r.is_empty());
        assert!(r.iter().any(|b| b.build.starts_with("26100")));
    }

    #[test]
    #[ignore]
    fn get_files_works() {
        // Feature update Windows 11 connue (vue lors de la recherche API).
        std::thread::sleep(Duration::from_secs(4));
        let r = get_files(
            "d7cd226f-dc7e-4edf-b2ec-bbffc7b975a0",
            "fr-fr",
            "PROFESSIONAL",
        )
        .expect("get files");
        assert!(!r.files.is_empty());
        assert!(r.files.values().any(|f| !f.url.is_empty()));
    }

    #[test]
    #[ignore]
    fn langs_and_editions_work() {
        let ids = list_ids("feature update", true).expect("search");
        let id = &ids[0].uuid;
        std::thread::sleep(Duration::from_secs(4));
        let langs = list_langs(id).expect("langs");
        assert!(!langs.list.is_empty());
        if let Some(l) = langs.list.first() {
            let eds = list_editions(l, id).expect("editions");
            assert!(!eds.list.is_empty() || eds.list.is_empty());
        }
    }

    #[test]
    fn percent_encode_basic() {
        assert_eq!(percent_encode("fr-fr"), "fr-fr");
        assert_eq!(percent_encode("a b"), "a+b");
        assert_eq!(percent_encode("a&b"), "a%26b");
    }
}

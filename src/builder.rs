//! Orchestration de la création d'ISO, fidèle au pipeline officiel UUP dump :
//!
//! 1. Récupère le package de conversion officiel (petit zip, `autodl=2`)
//! 2. Télécharge le convertisseur (7zr.exe + uup-converter-wimlib.7z, empreintes vérifiées)
//! 3. Extrait le convertisseur en préservant notre ConvertConfig.ini patché
//! 4. Passe les options de l'UI dans ConvertConfig.ini (AddUpdates, Cleanup, SkipEdge,
//!    AddDrivers…) et place les drivers dans Drivers/ALL (seul sous-dossier que le
//!    convertisseur officiel scanne avec OS/ et WinPE/ pour DISM /Add-Driver)
//! 5. Lance la conversion (élévation administrateur sous Windows) et surveille l'ISO
//! 6. Auto-clean : supprime les fichiers temporaires si demandé

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::models::BuildOptions;
use crate::util;

pub const SITE_BASE: &str = "https://uupdump.net";

/// Événements du processus de création.
#[derive(Debug, Clone)]
pub enum BuildEvent {
    Stage(String),
    Log(String),
    IsoFound(PathBuf),
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildContext {
    pub update_id: String,
    pub lang: String,
    pub editions: Vec<String>,
}

/// Entrée d'une liste "converter_*" (URL + nom + empreinte).
#[derive(Debug, Clone)]
struct ConvItem {
    url: String,
    name: String,
    sha256: String,
}

/// Exécute tout le pipeline dans un thread dédié.
pub fn spawn(
    ctx: BuildContext,
    options: BuildOptions,
    project_root: PathBuf,
    driver_dirs: Vec<String>,
    tx: Sender<BuildEvent>,
    cancel: std::sync::Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("uupbuild".into())
        .spawn(move || {
            let send = |e: BuildEvent| {
                let _ = tx.send(e);
            };
            let stage = |s: &str| send(BuildEvent::Stage(s.to_string()));
            let log = |s: String| send(BuildEvent::Log(s));

            let started = Instant::now();
            let _ = started;

            // ---------- 1. Package officiel ----------
            stage("Récupération du package de conversion officiel");
            let pkg_url = package_url(&ctx);
            log(format!("GET {pkg_url}"));
            let zip_bytes = match http_get_retries(&pkg_url, 4, &cancel) {
                Ok(b) => b,
                Err(e) => {
                    send(BuildEvent::Failed(format!(
                        "Package de conversion indisponible : {e}. Réessaie dans quelques minutes."
                    )));
                    return;
                }
            };
            let zip_path = project_root.join("uupdump_package.zip");
            if let Err(e) = std::fs::write(&zip_path, &zip_bytes) {
                send(BuildEvent::Failed(format!("Écriture du zip : {e}")));
                return;
            }
            if let Err(e) = util::extract_zip(&zip_path, &project_root) {
                send(BuildEvent::Failed(format!("Extraction du package : {e}")));
                return;
            }
            let _ = std::fs::remove_file(&zip_path);
            log("Package officiel extrait (ConvertConfig.ini, scripts, listes du convertisseur)".into());

            // ---------- 2. Convertisseur ----------
            stage("Téléchargement du convertisseur UUP");
            let list_file = if cfg!(target_os = "windows") {
                "converter_windows"
            } else {
                "converter_multi"
            };
            let list_path = project_root.join("files").join(list_file);
            let items = match parse_converter_list(&list_path) {
                Ok(i) => i,
                Err(e) => {
                    send(BuildEvent::Failed(e));
                    return;
                }
            };
            for it in &items {
                let dest = project_root.join("files").join(&it.name);
                log(format!("Téléchargement de {}…", it.name));
                if let Err(e) = fetch_file_sha256(&it.url, &dest, &it.sha256, &cancel) {
                    send(BuildEvent::Failed(format!("{} : {e}", it.name)));
                    return;
                }
            }

            // ---------- 3. Extraction ----------
            stage("Extraction du convertisseur");
            let conv_7z = items
                .iter()
                .find(|i| i.name.ends_with(".7z"))
                .map(|i| project_root.join("files").join(&i.name));
            let Some(conv_7z) = conv_7z else {
                send(BuildEvent::Failed("Archive du convertisseur introuvable dans la liste".into()));
                return;
            };
            let extract_ok: Result<(), String> = if cfg!(target_os = "windows") {
                let seven = project_root.join("files").join("7zr.exe");
                let out = std::process::Command::new(&seven)
                    .current_dir(&project_root)
                    .args(["-x!ConvertConfig.ini", "-x!CustomAppsList.txt", "-y", "x"])
                    .arg(&conv_7z)
                    .output();
                match out {
                    Ok(o) if o.status.success() => Ok(()),
                    Ok(o) => Err(format!(
                        "7zr.exe a échoué : {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    )),
                    Err(e) => Err(format!("lancement de 7zr.exe : {e}")),
                }
            } else {
                util::extract_7z_excluding_root(
                    &conv_7z,
                    &project_root,
                    &["ConvertConfig.ini", "CustomAppsList.txt"],
                )
                .map(|_| ())
            };
            if let Err(e) = extract_ok {
                send(BuildEvent::Failed(format!("Extraction du convertisseur : {e}")));
                return;
            }
            log("Convertisseur extrait (convert-UUP.cmd / convert.sh) avec notre configuration".into());

            // ---------- 4. Drivers + configuration ----------
            stage("Configuration (drivers, options)");
            if !driver_dirs.is_empty() {
                // IMPORTANT (source : convert-UUP.cmd officiel v126, routine :optDrivers) :
                // le script ne détecte les drivers QUE dans les sous-dossiers ALL/OS/WinPE
                // du dossier Drivers (recherche récursive des .inf) :
                //   ALL   → intégré aux images Windows (install.wim) ET aux images WinPE/WinRE
                //   OS    → uniquement les images Windows (install.wim)
                //   WinPE → uniquement les images de boot / WinRE
                // Un .inf posé directement dans Drivers/ ou dans un sous-dossier au nom
                // arbitraire est IGNORÉ par le convertisseur. On place donc tout dans ALL.
                let drivers_all = project_root.join("Drivers").join("ALL");
                let _ = std::fs::create_dir_all(&drivers_all);
                let mut inf_count = 0u64;
                for (i, d) in driver_dirs.iter().enumerate() {
                    let src = PathBuf::from(d);
                    if !src.is_dir() {
                        log(format!("⚠ Dossier de drivers ignoré (introuvable) : {d}"));
                        continue;
                    }
                    let name = src
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("driver_{i}"));
                    let dst = drivers_all.join(format!("{name}_{i}"));
                    match util::copy_dir(&src, &dst) {
                        Ok(n) => {
                            inf_count += count_inf(&dst);
                            log(format!(
                                "Drivers copiés : {d} → Drivers/ALL/{name} ({n} fichiers)"
                            ));
                        }
                        Err(e) => {
                            send(BuildEvent::Failed(format!("Copie des drivers ({d}) : {e}")));
                            return;
                        }
                    }
                }
                if inf_count == 0 {
                    log("⚠ Aucun fichier .inf trouvé dans les dossiers fournis : l'intégration sera sans effet.".into());
                } else {
                    log(format!(
                        "{inf_count} fichier(s) .inf prêt(s) — intégration DISM /Add-Driver /Recurse (AddDrivers=1)."
                    ));
                }
            }
            if let Err(e) = patch_convert_config(
                &project_root.join("ConvertConfig.ini"),
                &options,
                !driver_dirs.is_empty(),
            ) {
                send(BuildEvent::Failed(format!("Patch de ConvertConfig.ini : {e}")));
                return;
            }
            log("ConvertConfig.ini patché avec les options choisies".into());

            // ---------- 5. Conversion ----------
            if !options.make_iso {
                log("Conversion ISO désactivée : les fichiers UUP restent dans UUPs/.".into());
                send(BuildEvent::IsoFound(project_root.join("UUPs")));
                return;
            }
            stage("Création de l'ISO (fenêtre administrateur)");
            let stop_watch = std::sync::Arc::new(AtomicBool::new(false));
            {
                let stop_watch = std::sync::Arc::clone(&stop_watch);
                start_iso_watcher(project_root.clone(), tx.clone(), stop_watch);
            }
            let run_res = run_converter(&project_root, &options, &cancel, &tx);

            match run_res {
                Ok(()) => {
                    log("Processus de conversion terminé.".into());
                    std::thread::sleep(Duration::from_secs(4));
                    if let Some(iso) = find_iso(&project_root) {
                        send(BuildEvent::IsoFound(iso));
                    } else {
                        send(BuildEvent::Failed(
                            "Le convertisseur s'est terminé sans produire d'ISO. Consulte les messages de sa fenêtre (DISM/conversion).".into(),
                        ));
                        return;
                    }
                }
                Err(e) => {
                    send(BuildEvent::Failed(e));
                    return;
                }
            }
            stop_watch.store(true, Ordering::Relaxed);

            // ---------- 6. Auto-clean ----------
            if options.cleanup {
                stage("Auto-clean des fichiers temporaires");
                for sub in ["UUPs", "files", "__conv_tmp__", "ISODIR"] {
                    if let Err(e) = util::clean_dir(&project_root.join(sub)) {
                        log(format!("⚠ {e}"));
                    }
                }
                for f in ["7zr.exe", "uup-converter-wimlib.7z", "aria2_download.log"] {
                    let p = project_root.join("files").join(f);
                    if p.exists() {
                        let _ = std::fs::remove_file(&p);
                    }
                    let p2 = project_root.join(f);
                    if p2.exists() {
                        let _ = std::fs::remove_file(&p2);
                    }
                }
                log("Fichiers temporaires supprimés (UUPs/, files/).".into());
            }

            stage("Terminé");
        })
        .expect("spawn builder")
}

/// URL du package officiel (zip autodl) pour la sélection en cours.
fn package_url(ctx: &BuildContext) -> String {
    let mut url = format!("{SITE_BASE}/get.php?id={}", ctx.update_id);
    url.push_str(&format!("&pack={}", ctx.lang));
    if ctx.editions.len() == 1 {
        url.push_str(&format!("&edition={}", ctx.editions[0]));
    } else {
        for e in &ctx.editions {
            url.push_str(&format!("&edition%5B%5D={e}"));
        }
    }
    url.push_str("&autodl=2");
    url
}

fn http_get_retries(url: &str, attempts: usize, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .user_agent("uupdump-client-rs/0.1")
        .build();
    let mut last = String::new();
    for a in 0..attempts {
        if cancel.load(Ordering::Relaxed) {
            return Err("annulé".into());
        }
        if a > 0 {
            std::thread::sleep(Duration::from_secs(10 * a as u64).min(Duration::from_secs(45)));
        }
        match agent.get(url).call() {
            Ok(r) => {
                let mut buf = Vec::new();
                r.into_reader()
                    .read_to_end_err(&mut buf)
                    .map_err(|e| e.to_string())?;
                // Détection d'un marqueur d'erreur texte (#UUPDUMP_ERROR:...)
                if buf.starts_with(b"#UUPDUMP_ERROR:") {
                    let code = String::from_utf8_lossy(&buf[15..]).trim().to_string();
                    return Err(crate::api::api_translate(&code));
                }
                return Ok(buf);
            }
            Err(ureq::Error::Status(code, r)) => {
                let mut body = Vec::new();
                let _ = r.into_reader().read_to_end(&mut body);
                if let Ok(txt) = String::from_utf8(body.clone()) {
                    if let Some(c) = txt.strip_prefix("#UUPDUMP_ERROR:") {
                        return Err(crate::api::api_translate(c.trim()));
                    }
                }
                last = format!("HTTP {code}");
                if code == 429 || code >= 500 {
                    continue;
                }
                return Err(last);
            }
            Err(e) => {
                last = e.to_string();
            }
        }
    }
    Err(format!("échec après {attempts} tentatives ({last})"))
}

trait ReadToEndErr {
    fn read_to_end_err(&mut self, buf: &mut Vec<u8>) -> Result<(), String>;
}
impl<R: std::io::Read> ReadToEndErr for R {
    fn read_to_end_err(&mut self, buf: &mut Vec<u8>) -> Result<(), String> {
        self.read_to_end(buf).map(|_| ()).map_err(|e| e.to_string())
    }
}
use std::io::Read as _;

/// Télécharge un fichier et vérifie son SHA-256.
fn fetch_file_sha256(url: &str, dest: &Path, sha256: &str, cancel: &AtomicBool) -> Result<(), String> {
    if dest.exists() && !sha256.is_empty() {
        if let Ok(h) = sha256_file(dest) {
            if h.eq_ignore_ascii_case(sha256) {
                return Ok(()); // déjà présent et valide
            }
        }
        let _ = std::fs::remove_file(dest);
    }
    let bytes = http_get_retries(url, 3, cancel)?;
    std::fs::write(dest, &bytes).map_err(|e| format!("écriture : {e}"))?;
    if !sha256.is_empty() {
        let h = sha256_file(dest)?;
        if !h.eq_ignore_ascii_case(sha256) {
            return Err(format!("empreinte SHA-256 invalide (attendu {})", &sha256[..12]));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).map_err(|e| format!("ouverture : {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_str(&hasher.finalize()))
}

fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse une liste "converter_windows" / "converter_multi" (format aria2).
fn parse_converter_list(path: &Path) -> Result<Vec<ConvItem>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{} : {e}", path.display()))?;
    if let Some(c) = text.trim().strip_prefix("#UUPDUMP_ERROR:") {
        return Err(crate::api::api_translate(c.trim()));
    }
    let mut items = Vec::new();
    let mut current: Option<ConvItem> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            if let Some(c) = current.take() {
                items.push(c);
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("out=") {
            if let Some(c) = current.as_mut() {
                c.name = rest.to_string();
            }
        } else if let Some(rest) = line.strip_prefix("checksum=sha-256=") {
            if let Some(c) = current.as_mut() {
                c.sha256 = rest.to_string();
            }
        } else if line.starts_with("http://") || line.starts_with("https://") {
            current = Some(ConvItem {
                url: line.to_string(),
                name: String::new(),
                sha256: String::new(),
            });
        }
    }
    if let Some(c) = current.take() {
        items.push(c);
    }
    items.retain(|i| !i.name.is_empty());
    if items.is_empty() {
        return Err("Liste du convertisseur vide ou illisible".into());
    }
    Ok(items)
}

/// Compte récursivement les fichiers .inf d'un dossier.
fn count_inf(dir: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += count_inf(&p);
            } else if p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("inf"))
                .unwrap_or(false)
            {
                n += 1;
            }
        }
    }
    n
}

/// Applique les options de l'UI dans ConvertConfig.ini (clés officielles).
fn patch_convert_config(path: &Path, options: &BuildOptions, has_drivers: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("lecture : {e}"))?;
    let mut values: Vec<(&str, String)> = vec![
        ("AutoStart", "1".into()),
        ("AutoExit", "1".into()),
        ("AddUpdates", bool_i(options.add_updates)),
        ("Cleanup", bool_i(options.cleanup)),
        ("ResetBase", bool_i(options.reset_base)),
        ("SkipEdge", bool_i(options.skip_edge)),
        ("wim2esd", bool_i(options.esd)),
        ("StartVirtual", bool_i(options.virtual_editions)),
        ("AddDrivers", i(has_drivers)),
        ("Drv_Source", "\\Drivers".into()),
    ];
    if options.virtual_editions {
        values.push(("vAutoStart", "1".into()));
        values.push(("vUseDism", "1".into()));
    }

    let mut out = String::with_capacity(text.len() + 128);
    let mut section = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed.to_string();
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut replaced = false;
        if section == "[convert-UUP]" || section == "[create_virtual_editions]" {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if let Some((_, val)) = values.iter().find(|(k, _)| *k == key) {
                    // On conserve l'alignement de la clé, on remplace la valeur.
                    if let Some(pos) = line.find('=') {
                        out.push_str(&format!("{}= {}\n", &line[..pos], val));
                        replaced = true;
                    }
                }
                values.retain(|(k, _)| *k != key);
            }
        }
        if !replaced {
            out.push_str(line);
            out.push('\n');
        }
    }
    // Les clés non trouvées sont ignorées : le ConvertConfig.ini officiel
    // contient déjà l'intégralité des clés utilisées.
    std::fs::write(path, out.trim_end().to_string() + "\n").map_err(|e| format!("écriture : {e}"))
}

fn bool_i(b: bool) -> String {
    if b { "1".into() } else { "0".into() }
}
fn i(b: bool) -> String {
    bool_i(b)
}

/// Lance le convertisseur nativement.
fn run_converter(
    root: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
    tx: &Sender<BuildEvent>,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let script = root.join("convert-UUP.cmd");
        if !script.exists() {
            return Err("convert-UUP.cmd introuvable après extraction".into());
        }
        let root_str = root.to_string_lossy().to_string();
        let ps = format!(
            "Start-Process -FilePath '{}' -WorkingDirectory '{}' -Verb RunAs -Wait",
            script.to_string_lossy().replace('\'', "''"),
            root_str.replace('\'', "''")
        );
        let _ = tx.send(BuildEvent::Log(
            "Une fenêtre de contrôle de compte utilisateur peut apparaître : valide l'exécution en tant qu'administrateur.".into(),
        ));
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
            .status()
            .map_err(|e| format!("lancement PowerShell : {e}"))?;
        if cancel.load(Ordering::Relaxed) {
            return Err("annulé".into());
        }
        if !status.success() {
            return Err(format!("Le convertisseur s'est arrêté avec le code {:?}", status.code()));
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = cancel;
        let script = root.join("convert.sh");
        if !script.exists() {
            return Err("convert.sh introuvable après extraction (le convertisseur multiplateforme est attendu)".into());
        }
        // Pré-vérification des outils requis par le convertisseur officiel.
        let missing: Vec<&str> = ["aria2c", "cabextract", "wimlib-imagex", "chntpw", "mkisofs"]
            .into_iter()
            .filter(|p| which(p).is_none())
            .collect();
        let missing = if missing.contains(&"mkisofs") {
            missing
                .into_iter()
                .filter(|p| *p != "mkisofs" || which("genisoimage").is_none())
                .collect()
        } else {
            missing
        };
        if !missing.is_empty() {
            return Err(format!(
                "Outils manquants pour la conversion : {}. Installe-les (ex. : sudo apt-get install aria2 cabextract wimtools chntpw genisoimage) puis relance.",
                missing.join(", ")
            ));
        }
        let comp = if options.esd { "esd" } else { "wim" };
        let ve = if options.virtual_editions { "1" } else { "0" };
        let _ = tx.send(BuildEvent::Log(format!(
            "Exécution : convert.sh {comp} UUPs {ve}"
        )));
        let status = std::process::Command::new("bash")
            .arg("convert.sh")
            .arg(comp)
            .arg("UUPs")
            .arg(ve)
            .current_dir(root)
            .status()
            .map_err(|e| format!("lancement convert.sh : {e}"))?;
        if !status.success() {
            return Err(format!("Le convertisseur s'est arrêté avec le code {:?}", status.code()));
        }
        Ok(())
    }
}

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(target_os = "windows")]
        let candidate = dir.join(format!("{prog}.exe"));
        #[cfg(target_os = "windows")]
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Surveille l'apparition d'un fichier .ISO dans le dossier du projet.
fn start_iso_watcher(root: PathBuf, tx: Sender<BuildEvent>, stop: std::sync::Arc<AtomicBool>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if let Some(p) = find_iso(&root) {
                let _ = tx.send(BuildEvent::IsoFound(p));
                return;
            }
        }
    });
}

fn find_iso(root: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(root).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in rd.flatten() {
        let p = e.path();
        let is_iso = p
            .extension()
            .map(|x| x.eq_ignore_ascii_case("iso"))
            .unwrap_or(false);
        if !is_iso {
            continue;
        }
        if let Ok(md) = e.metadata() {
            if md.len() > 0 {
                let modified = md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                    best = Some((modified, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn parse_converter_list_ok() {
        let p = std::env::temp_dir().join(format!("conv_test_{}.txt", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "https://example.org/a.exe").unwrap();
        writeln!(f, "  out=7zr.exe").unwrap();
        writeln!(f, "  checksum=sha-256=abc123").unwrap();
        let items = parse_converter_list(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "7zr.exe");
        assert_eq!(items[0].sha256, "abc123");
    }

    #[test]
    fn parse_converter_list_error_marker() {
        let p = std::env::temp_dir().join(format!("conv_err_{}.txt", std::process::id()));
        std::fs::write(&p, "#UUPDUMP_ERROR:USER_RATE_LIMITED").unwrap();
        let err = parse_converter_list(&p).unwrap_err();
        let _ = std::fs::remove_file(&p);
        assert!(err.contains("Trop de requêtes"));
    }

    #[test]
    fn patch_config_sets_flags() {
        let p = std::env::temp_dir().join(format!("cfg_test_{}.ini", std::process::id()));
        std::fs::write(
            &p,
            "[convert-UUP]\nAutoStart    =0\nAddUpdates   =0\n[Store_Apps]\nSkipApps     =0\n",
        )
        .unwrap();
        let mut opts = BuildOptions::default();
        opts.add_updates = true;
        patch_convert_config(&p, &opts, false).unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert!(txt.contains("AutoStart    = 1"), "{txt}");
        assert!(txt.contains("AddUpdates   = 1"), "{txt}");
        assert!(txt.contains("SkipApps     =0"), "{txt}");
    }

    #[test]
    fn patch_config_drivers_and_virtual_editions() {
        // Structure officielle du ConvertConfig.ini (convertisseur v126)
        let p = std::env::temp_dir().join(format!("cfg_drv_{}.ini", std::process::id()));
        std::fs::write(
            &p,
            "[convert-UUP]\nAutoStart    =0\nAddUpdates   =0\nAddDrivers   =0\nDrv_Source   =\\Drivers\n[Store_Apps]\nSkipApps     =0\n[create_virtual_editions]\nvUseDism     =1\nvAutoStart   =1\n",
        )
        .unwrap();
        let opts = BuildOptions {
            virtual_editions: true,
            ..Default::default()
        };
        patch_convert_config(&p, &opts, true).unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert!(txt.contains("AddDrivers   = 1"), "{txt}");
        assert!(txt.contains("Drv_Source   = \\Drivers"), "{txt}");
        // La section [create_virtual_editions] doit aussi être patchée
        assert!(txt.contains("vUseDism     = 1"), "{txt}");
        assert!(txt.contains("vAutoStart   = 1"), "{txt}");
        assert!(txt.contains("SkipApps     =0"), "{txt}");
    }

    #[test]
    fn count_inf_recursive() {
        let base = std::env::temp_dir().join(format!("drv_inf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("gpu").join("x64");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("readme.txt"), "x").unwrap();
        std::fs::write(base.join("root.inf"), "x").unwrap();
        std::fs::write(nested.join("deep.INF"), "x").unwrap();
        assert_eq!(count_inf(&base), 2);
        let _ = std::fs::remove_dir_all(&base);
    }
}

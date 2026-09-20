//! Orchestration d'un job : récupération de la liste de fichiers, téléchargement
//! (aria2c ou téléchargeur natif), préparation du convertisseur officiel UUP dump,
//! injection des drivers, création de l'ISO et nettoyage.
//!
//! Le pipeline reproduit fidèlement le script `uup_download_windows.cmd` généré
//! par uupdump.net (voir research/contrib_get.php) :
//!   1. fichiers UUP téléchargés dans  UUPs/
//!   2. convertisseur officiel extrait (uup-converter-wimlib.7z via 7zr.exe)
//!   3. ConvertConfig.ini écrit avec les options choisies
//!   4. drivers copiés dans Drivers/ALL | Drivers/OS | Drivers/WinPE
//!   5. convert-UUP.cmd lancé (auto-élévation UAC, ISO écrite à la racine du dossier)

use crate::api::{self, FileInfo, PackageFiles};
use crate::store::{Compression, DriverEntry, PackageOptions};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Phase {
    #[default]
    Idle,
    FetchingList,
    Preparing,
    Downloading,
    Converting,
    CleaningUp,
    Done,
    Failed(String),
}

pub struct JobRequest {
    pub update_id: String,
    pub title: String,
    pub build: String,
    pub arch: String,
    pub lang: String,
    pub lang_fancy: String,
    pub edition: String,
    pub edition_fancy: String,
    pub options: PackageOptions,
    pub drivers: Vec<DriverEntry>,
    pub output_dir: String,
}

#[derive(Default)]
pub struct JobState {
    pub phase: Phase,
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed_bps: u64,
    pub log: Vec<String>,
    pub iso_path: Option<String>,
    pub work_dir: Option<String>,
}

pub type SharedJob = Arc<Mutex<JobState>>;
pub type CancelFlag = Arc<AtomicBool>;

const LOG_CAP: usize = 500;

fn push_log(st: &SharedJob, line: String) {
    let mut s = st.lock().unwrap();
    s.log.push(line);
    if s.log.len() > LOG_CAP {
        let overflow = s.log.len() - LOG_CAP;
        s.log.drain(0..overflow);
    }
}

fn set_phase(st: &SharedJob, p: Phase) {
    st.lock().unwrap().phase = p;
}

fn log_line(st: &SharedJob, line: impl Into<String>) {
    let line = line.into();
    println!("[job] {line}");
    push_log(st, line);
}

// ---------------------------------------------------------------- SHA-256

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------- Téléchargement utilitaire

fn download_to_file(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    expect_sha256: Option<&str>,
    _st: &SharedJob,
    cancel: &CancelFlag,
) -> Result<(), String> {
    let tmp = dest.with_extension("part");
    let mut last_err = String::new();
    for attempt in 1..=3 {
        if cancel.load(Ordering::Relaxed) {
            return Err("annulé".into());
        }
        let resp = match agent.get(url).call() {
            Ok(r) => r,
            Err(e) => {
                last_err = e.to_string();
                std::thread::sleep(Duration::from_secs(2 * attempt as u64));
                continue;
            }
        };
        let total = resp
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let mut reader = resp.into_reader();
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 65536];
        let result = loop {
            if cancel.load(Ordering::Relaxed) {
                break Err("annulé".into());
            }
            match reader.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    if file.write_all(&buf[..n]).is_err() {
                        break Err("écriture disque impossible".into());
                    }
                    let _ = total; // progression détaillée gérée par le sampling du dossier
                }
                Err(e) => break Err(e.to_string()),
            }
        };
        drop(file);
        match result {
            Ok(()) => {
                if let Some(want) = expect_sha256 {
                    let got = sha256_file(&tmp).map_err(|e| e.to_string())?;
                    if got.to_lowercase() != want.to_lowercase() {
                        last_err = format!("empreinte SHA-256 invalide pour {}", dest.display());
                        let _ = fs::remove_file(&tmp);
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                }
                fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(e) => {
                last_err = e;
                let _ = fs::remove_file(&tmp);
                if cancel.load(Ordering::Relaxed) {
                    return Err("annulé".into());
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    Err(last_err)
}

// ---------------------------------------------------------------- job principal

pub fn spawn(req: JobRequest, st: SharedJob, cancel: CancelFlag) {
    std::thread::Builder::new()
        .name("uup-job".into())
        .spawn(move || run_job(req, st, cancel))
        .expect("spawn job thread");
}

fn run_job(req: JobRequest, st: SharedJob, cancel: CancelFlag) {
    let agent: ureq::Agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .user_agent("uupdump-client-rs/0.1")
        .build();

    let is_windows = cfg!(target_os = "windows");

    // Nom de dossier lisible et sans caractères interdits.
    let folder = sanitize(&format!(
        "{}_{}_{}",
        req.build.replace('.', "_"),
        req.lang,
        if req.edition == "0" {
            "all"
        } else {
            &req.edition
        }
    ));
    let work = PathBuf::from(&req.output_dir).join(&folder);
    let uups = work.join("UUPs");
    let files_dir = work.join("files");
    let drivers_dir = work.join("Drivers");

    {
        let mut s = st.lock().unwrap();
        s.work_dir = Some(work.to_string_lossy().into_owned());
        s.phase = Phase::FetchingList;
        s.log.clear();
        s.files_done = 0;
        s.bytes_done = 0;
        s.iso_path = None;
    }

    let finish_fail = |st: &SharedJob, msg: String| {
        let mut s = st.lock().unwrap();
        s.phase = Phase::Failed(msg);
    };

    for d in [&work, &uups, &files_dir] {
        if let Err(e) = fs::create_dir_all(d) {
            finish_fail(&st, format!("Impossible de créer {}: {e}", d.display()));
            return;
        }
    }

    // ------------------------------------------------ 1. Liste de fichiers
    log_line(
        &st,
        format!(
            "Build : {} [{}] — éd. {} ({}) — langue {} ({})",
            req.build,
            req.arch,
            req.edition,
            if req.edition_fancy.is_empty() {
                "?".to_string()
            } else {
                req.edition_fancy.clone()
            },
            req.lang,
            req.lang_fancy,
        ),
    );
    log_line(&st, format!("Update {} — {}", req.update_id, req.title));
    log_line(&st, format!("Récupération de la liste de fichiers…"));
    let list: PackageFiles = match api::get_files(&req.update_id, &req.lang, &req.edition) {
        Ok(l) => l,
        Err(e) => {
            finish_fail(&st, format!("{e}"));
            return;
        }
    };
    if list.files.is_empty() {
        finish_fail(&st, "Aucun fichier retourné pour cette combinaison.".into());
        return;
    }
    log_line(
        &st,
        format!(
            "Package « {} » ({} {}) — {} fichiers, taille totale ~{}",
            list.update_name,
            list.build,
            list.arch,
            list.files.len(),
            human_size(list.files.values().map(|f| f.size.max(0) as u64).sum())
        ),
    );

    {
        let mut s = st.lock().unwrap();
        s.files_total = list.files.len();
        s.bytes_total = list.files.values().map(|f| f.size.max(0) as u64).sum();
    }

    // Sauvegarde la liste pour traçabilité
    let _ =
        serde_json::to_string_pretty(&list.files).map(|j| fs::write(work.join("filelist.json"), j));

    // ------------------------------------------------ 2. Convertisseur officiel
    if req.options.convert_iso {
        set_phase(&st, Phase::Preparing);
        log_line(&st, "Téléchargement du convertisseur officiel UUP dump…");
        if is_windows {
            let seven = files_dir.join("7zr.exe");
            let conv = files_dir.join("uup-converter-wimlib.7z");
            if let Err(e) = download_to_file(
                &agent,
                api::SEVEN_ZR_URL,
                &seven,
                Some(api::SEVEN_ZR_SHA256),
                &st,
                &cancel,
            ) {
                finish_fail(&st, format!("7zr.exe : {e}"));
                return;
            }
            if let Err(e) = download_to_file(
                &agent,
                api::CONVERTER_7Z_URL,
                &conv,
                Some(api::CONVERTER_7Z_SHA256),
                &st,
                &cancel,
            ) {
                finish_fail(&st, format!("Convertisseur : {e}"));
                return;
            }
            log_line(&st, "Extraction du convertisseur…");
            let out = std::process::Command::new(&seven)
                .arg("-y")
                .arg("x")
                .arg(&conv)
                .arg("-o")
                .arg(&work)
                .output();
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    finish_fail(
                        &st,
                        format!(
                            "Extraction 7z échouée : {}",
                            String::from_utf8_lossy(&o.stderr).trim()
                        ),
                    );
                    return;
                }
                Err(e) => {
                    finish_fail(&st, format!("Extraction 7z : {e}"));
                    return;
                }
            }
        } else {
            // Linux / macOS : scripts du convertisseur multiplateforme (commit épinglé par UUP dump).
            let base = "https://git.uupdump.net/uup-dump/converter/raw/commit/65030a1e6928dc05b163e49cf23882eb8fefccd4";
            let sh = files_dir.join("convert.sh");
            let ve = files_dir.join("convert_ve_plugin");
            if let Err(e) = download_to_file(
                &agent,
                &format!("{base}/convert.sh"),
                &sh,
                Some("fde98886a8d0c15de9c685a3b5bd19e66ddbbcde9fda634c7af1aa0720c2b9fc"),
                &st,
                &cancel,
            ) {
                finish_fail(&st, format!("convert.sh : {e}"));
                return;
            }
            if let Err(e) = download_to_file(
                &agent,
                &format!("{base}/convert_ve_plugin"),
                &ve,
                Some("02db2f5f2caf742daa6aeaa189d9af27775e8457db48fadd8d71bb1be5982eae"),
                &st,
                &cancel,
            ) {
                finish_fail(&st, format!("convert_ve_plugin : {e}"));
                return;
            }
            let _ = std::process::Command::new("chmod")
                .arg("+x")
                .arg(&sh)
                .status();
        }
        log_line(&st, "Convertisseur prêt.");
    }

    // ------------------------------------------------ 3. ConvertConfig.ini + drivers
    if is_windows {
        let add_drivers = !req.drivers.is_empty();
        let ini = render_convert_config(&req.options, add_drivers);
        if let Err(e) = fs::write(work.join("ConvertConfig.ini"), ini) {
            finish_fail(&st, format!("ConvertConfig.ini : {e}"));
            return;
        }
    }

    if req.options.convert_iso && !req.drivers.is_empty() {
        log_line(&st, "Copie des drivers…");
        for d in &req.drivers {
            let src = PathBuf::from(&d.path);
            if !src.exists() {
                log_line(&st, format!("⚠ Dossier introuvable, ignoré : {}", d.path));
                continue;
            }
            let dst = drivers_dir.join(d.target.folder());
            if let Err(e) = copy_dir_recursive(&src, &dst) {
                log_line(&st, format!("⚠ Copie drivers échouée ({}): {e}", d.path));
            } else {
                log_line(&st, format!("→ {} [{}]", d.path, d.target.label()));
            }
        }
    }

    // ------------------------------------------------ 4. Téléchargement des fichiers UUP
    set_phase(&st, Phase::Downloading);
    log_line(&st, "Préparation du téléchargement…");

    let aria_input = work.join("aria2_input.txt");
    let mut input = String::new();
    for (name, info) in &list.files {
        if info.url.is_empty() {
            continue;
        }
        input.push_str(&info.url);
        input.push('\n');
        input.push_str(&format!("  out={name}\n"));
        if let Some(sha) = &info.sha256 {
            if !sha.is_empty() {
                input.push_str(&format!("  checksum=sha-256={}\n", sha.to_lowercase()));
            }
        }
        input.push('\n');
    }
    if let Err(e) = fs::write(&aria_input, input) {
        finish_fail(&st, format!("aria2_input.txt : {e}"));
        return;
    }

    let dl_ok = if is_windows {
        download_with_aria2_windows(
            &agent,
            &work,
            &files_dir,
            &aria_input,
            &uups,
            &list.files,
            &st,
            &cancel,
        )
    } else {
        let ok = which("aria2c").is_some();
        if ok {
            download_with_aria2_unix(&aria_input, &uups, &st, &cancel)
        } else {
            log_line(
                &st,
                "aria2c absent : téléchargement natif (installez aria2 pour de meilleures performances)",
            );
            download_native(&list.files, &uups, &st, &cancel)
        }
    };

    if cancel.load(Ordering::Relaxed) {
        set_phase(&st, Phase::Idle);
        log_line(&st, "Téléchargement annulé.");
        return;
    }
    if !dl_ok {
        finish_fail(&st, "Le téléchargement a échoué (voir journal).".into());
        return;
    }

    // Vérification rapide : tous les fichiers présents ?
    let mut missing = 0;
    for name in list.files.keys() {
        if !uups.join(name).exists() {
            missing += 1;
        }
    }
    if missing > 0 {
        finish_fail(
            &st,
            format!("{missing} fichier(s) manquant(s) après téléchargement — relancez le job (reprise automatique)."),
        );
        return;
    }
    log_line(
        &st,
        "Téléchargement terminé, intégrité vérifiée par aria2/native.",
    );

    // ------------------------------------------------ 5. Conversion ISO
    if req.options.convert_iso {
        set_phase(&st, Phase::Converting);
        let started = Instant::now();
        if is_windows {
            log_line(&st, "Lancement de convert-UUP.cmd (une fenêtre de conversion s'ouvre ; validez l'élévation UAC si demandée)…");
            let child = spawn_cmd_hidden_console(&work);

            match child {
                Ok(mut _c) => {
                    // Le script s'auto-élève : le processus initial se ferme vite.
                    // On surveille l'apparition de l'ISO à la racine du dossier.
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            log_line(&st, "Annulé : fermez manuellement la fenêtre de conversion si elle est ouverte.");
                            set_phase(&st, Phase::Idle);
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        if let Some(iso) = find_first_iso(&work) {
                            let mut s = st.lock().unwrap();
                            s.iso_path = Some(iso.to_string_lossy().into_owned());
                            drop(s);
                            log_line(&st, format!("ISO générée : {}", iso.display()));
                            break;
                        }
                        // Progression indicative : taille de ISOFOLDER
                        let iso_folder = work.join("ISOFOLDER");
                        if iso_folder.exists() {
                            let sz = dir_size(&iso_folder);
                            let mut s = st.lock().unwrap();
                            s.bytes_done = sz;
                            if s.bytes_total == 0 {
                                s.bytes_total = 5 * 1024 * 1024 * 1024; // ~DVD
                            }
                        }
                        let _ = started.elapsed();
                    }
                }
                Err(e) => {
                    finish_fail(&st, format!("Lancement convert-UUP.cmd : {e}"));
                    return;
                }
            }
        } else {
            log_line(&st, "Conversion via convert.sh (wimlib)…");
            let ctype = if req.options.compression == Compression::Esd {
                "esd"
            } else {
                "wim"
            };
            let vflag = if req.options.virtual_editions {
                "1"
            } else {
                "0"
            };
            let cfg = files_dir.join("convert_config_linux");
            let _ = fs::write(&cfg, format!("VIRTUAL_EDITIONS_LIST=''\n"));
            let status = std::process::Command::new("bash")
                .arg(files_dir.join("convert.sh"))
                .arg(ctype)
                .arg(&uups)
                .arg(vflag)
                .current_dir(&work)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            match status {
                Ok(mut child) => {
                    if let Some(stdout) = child.stdout.take() {
                        let st2 = st.clone();
                        std::thread::spawn(move || {
                            let reader = std::io::BufReader::new(stdout);
                            for line in reader.lines().flatten() {
                                push_log(&st2, line);
                            }
                        });
                    }
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = child.kill();
                            set_phase(&st, Phase::Idle);
                            return;
                        }
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                if status.success() {
                                    break;
                                } else {
                                    finish_fail(&st, format!("convert.sh a échoué ({status})."));
                                    return;
                                }
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(800)),
                            Err(e) => {
                                finish_fail(&st, format!("convert.sh : {e}"));
                                return;
                            }
                        }
                    }
                    if let Some(iso) = find_first_iso(&work) {
                        let mut s = st.lock().unwrap();
                        s.iso_path = Some(iso.to_string_lossy().into_owned());
                    }
                }
                Err(e) => {
                    finish_fail(&st, format!("Lancement convert.sh : {e}"));
                    return;
                }
            }
        }
    } else {
        log_line(
            &st,
            "Conversion ISO désactivée : les fichiers UUP sont prêts dans UUPs/.",
        );
    }

    // ------------------------------------------------ 6. Auto-clean
    if req.options.auto_clean {
        set_phase(&st, Phase::CleaningUp);
        log_line(&st, "Nettoyage automatique des fichiers temporaires…");
        let _ = fs::remove_dir_all(&uups);
        let _ = fs::remove_dir_all(work.join("temp"));
        let _ = fs::remove_dir_all(work.join("ISOFOLDER"));
        let _ = fs::remove_file(work.join("aria2_input.txt"));
        let _ = fs::remove_file(files_dir.join("uup-converter-wimlib.7z"));
        let _ = fs::remove_file(files_dir.join("7zr.exe"));
        let _ = fs::remove_file(work.join("aria2_download.log"));
        log_line(&st, "Nettoyage terminé.");
    }

    {
        let mut s = st.lock().unwrap();
        s.phase = Phase::Done;
        s.bytes_done = s.bytes_total;
        s.files_done = s.files_total;
    }
    log_line(&st, "✔ Terminé.");
}

// ------------------------------------------------ aria2 (Windows : binaire téléchargé dans files/)

fn download_with_aria2_windows(
    agent: &ureq::Agent,
    work: &Path,
    files_dir: &Path,
    aria_input: &Path,
    uups: &Path,
    files: &HashMap<String, FileInfo>,
    st: &SharedJob,
    cancel: &CancelFlag,
) -> bool {
    let exe = files_dir.join("aria2c.exe");
    if !exe.exists() {
        log_line(st, "Téléchargement d'aria2c.exe…");
        if let Err(e) = download_to_file(agent, api::ARIA2C_URL, &exe, None, st, cancel) {
            log_line(
                st,
                format!("⚠ aria2c indisponible ({e}) : téléchargeur natif utilisé."),
            );
            return download_native(files, uups, st, cancel);
        }
    }
    run_aria2(&exe, work, aria_input, uups, st, cancel)
}

fn download_with_aria2_unix(
    aria_input: &Path,
    uups: &Path,
    st: &SharedJob,
    cancel: &CancelFlag,
) -> bool {
    let path = which("aria2c")
        .map(|p| PathBuf::from(p))
        .unwrap_or_else(|| PathBuf::from("aria2c"));
    run_aria2(
        &path,
        uups.parent().unwrap_or(Path::new(".")),
        aria_input,
        uups,
        st,
        cancel,
    )
}

fn run_aria2(
    exe: &Path,
    work: &Path,
    aria_input: &Path,
    uups: &Path,
    st: &SharedJob,
    cancel: &CancelFlag,
) -> bool {
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "--no-conf",
        "--async-dns=false",
        "--console-log-level=warn",
        "--summary-interval=5",
        "--max-tries=5",
        "--retry-wait=3",
        "--check-integrity=true",
        "--auto-file-renaming=false",
        "--allow-overwrite=true",
        "--file-allocation=none",
        "-x16",
        "-s16",
        "-j5",
        "-c",
        "-R",
    ])
    .arg("-d")
    .arg(uups)
    .arg("-i")
    .arg(aria_input)
    .current_dir(work)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());

    let child = cmd.spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            log_line(st, format!("⚠ Impossible de lancer aria2 ({e})."));
            return false;
        }
    };

    if let Some(stdout) = child.stdout.take() {
        let st2 = st.clone();
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines().flatten() {
                let l = line.trim().to_string();
                if l.is_empty() {
                    continue;
                }
                push_log(&st2, l);
            }
        });
    }

    let start = Instant::now();
    let mut last_bytes: u64 = 0;
    let mut last_t = start;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            return false;
        }
        std::thread::sleep(Duration::from_millis(700));
        let bytes = dir_size(uups);
        {
            let mut s = st.lock().unwrap();
            s.bytes_done = bytes;
            let dt = last_t.elapsed().as_secs_f64().max(0.001);
            s.speed_bps = ((bytes.saturating_sub(last_bytes)) as f64 / dt) as u64;
        }
        last_bytes = bytes;
        last_t = Instant::now();
        match child.try_wait() {
            Ok(Some(status)) => {
                let speed = 0;
                st.lock().unwrap().speed_bps = speed;
                if status.success() {
                    // Recompte précis
                    let done = std::fs::read_dir(uups)
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .filter(|e| {
                                    e.path().is_file()
                                        && e.path()
                                            .extension()
                                            .map(|x| x != "aria2")
                                            .unwrap_or(true)
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    let mut s = st.lock().unwrap();
                    s.files_done = done;
                    return true;
                }
                log_line(
                    st,
                    format!("aria2 s'est arrêté ({status}) — reprise possible en relançant."),
                );
                return false;
            }
            Ok(None) => {}
            Err(e) => {
                log_line(st, format!("aria2 : {e}"));
                return false;
            }
        }
    }
}

// ------------------------------------------------ Téléchargeur natif de secours

fn download_native(
    files: &HashMap<String, FileInfo>,
    uups: &Path,
    st: &SharedJob,
    cancel: &CancelFlag,
) -> bool {
    let agent: ureq::Agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(300))
        .user_agent("uupdump-client-rs/0.1")
        .build();

    let jobs: Vec<(String, FileInfo)> = files
        .iter()
        .filter(|(_, i)| !i.url.is_empty())
        .map(|(n, i)| (n.clone(), i.clone()))
        .collect();

    let shared: Arc<(Mutex<u64>, Mutex<u64>)> = Arc::new((Mutex::new(0), Mutex::new(0))); // (octets, fichiers ok)
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ok_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let err_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for _w in 0..4.min(jobs.len().max(1)) {
            let agent = &agent;
            let next = &next;
            let ok_count = &ok_count;
            let err_count = &err_count;
            let shared = &shared;
            let st = &st;
            let cancel = &cancel;
            let jobs = &jobs;
            let uups = &uups;
            scope.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= jobs.len() {
                    return;
                }
                let (name, info) = &jobs[i];
                let dest = uups.join(sanitize(name));
                if dest.exists()
                    && info.size > 0
                    && dest.metadata().map(|m| m.len()).unwrap_or(0) == info.size as u64
                {
                    ok_count.fetch_add(1, Ordering::SeqCst);
                    *shared.0.lock().unwrap() += info.size.max(0) as u64;
                    *shared.1.lock().unwrap() += 1;
                    continue;
                }
                match fetch_one(&agent, &info.url, &dest, info.sha256.as_deref()) {
                    Ok(()) => {
                        ok_count.fetch_add(1, Ordering::SeqCst);
                        *shared.0.lock().unwrap() += info.size.max(0) as u64;
                        let oc = ok_count.load(Ordering::SeqCst);
                        let mut s = st.lock().unwrap();
                        s.files_done = oc;
                        s.bytes_done = *shared.0.lock().unwrap();
                    }
                    Err(e) => {
                        err_count.fetch_add(1, Ordering::SeqCst);
                        push_log(st, format!("✗ {name} : {e}"));
                    }
                }
            });
        }
    });

    let errors = err_count.load(Ordering::SeqCst);
    if errors > 0 {
        push_log(st, format!("{errors} fichier(s) en échec."));
        return false;
    }
    true
}

fn fetch_one(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    sha256: Option<&str>,
) -> Result<(), String> {
    let tmp = dest.with_extension("part");
    let mut last = String::new();
    for _ in 0..3 {
        let resp = agent.get(url).call().map_err(|e| e.to_string())?;
        let mut reader = resp.into_reader();
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 65536];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => file.write_all(&buf[..n]).map_err(|e| e.to_string())?,
                Err(e) => return Err(e.to_string()),
            }
        }
        drop(file);
        if let Some(want) = sha256 {
            if !want.is_empty() {
                let got = sha256_file(&tmp).map_err(|e| e.to_string())?;
                if got != want.to_lowercase() {
                    last = "SHA-256 invalide".into();
                    let _ = fs::remove_file(&tmp);
                    continue;
                }
            }
        }
        fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
        return Ok(());
    }
    Err(last)
}

// ------------------------------------------------ helpers

/// Lance `cmd /C convert-UUP.cmd` dans une nouvelle console visible (Windows).
#[cfg(target_os = "windows")]
fn spawn_cmd_hidden_console(work: &Path) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    std::process::Command::new("cmd")
        .args(["/C", "convert-UUP.cmd"])
        .current_dir(work)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
}

#[cfg(not(target_os = "windows"))]
fn spawn_cmd_hidden_console(_work: &Path) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::other("conversion cmd réservée à Windows"))
}

fn render_convert_config(opts: &PackageOptions, add_drivers: bool) -> String {
    let autostart = if opts.compression == Compression::Esd {
        2
    } else {
        1
    };
    let wim2esd = if opts.compression == Compression::Esd {
        1
    } else {
        0
    };
    let add_updates = opts.add_updates as i32;
    let cleanup = opts.cleanup as i32;
    let reset_base = opts.reset_base as i32;
    let netfx3 = opts.netfx3 as i32;
    let start_virtual = opts.virtual_editions as i32;
    let skip_edge = opts.skip_edge as i32;
    let skip_winre = opts.skip_winre as i32;
    // NOTE : convert_iso=false court-circuite la conversion côté appli ; SkipISO reste 0.
    format!(
        "[convert-UUP]\n\
         AutoStart    ={autostart}\n\
         AddUpdates   ={add_updates}\n\
         Cleanup      ={cleanup}\n\
         ResetBase    ={reset_base}\n\
         NetFx3       ={netfx3}\n\
         StartVirtual ={start_virtual}\n\
         wim2esd      ={wim2esd}\n\
         wim2swm      =0\n\
         SkipISO      =0\n\
         SkipWinRE    ={skip_winre}\n\
         LCUwinre     =0\n\
         LCUmsuExpand =0\n\
         UpdtBootFiles=0\n\
         ForceDism    =0\n\
         RefESD       =0\n\
         SkipLCUmsu   =0\n\
         SkipEdge     ={skip_edge}\n\
         AutoExit     =1\n\
         DisableUpdatingUpgrade=0\n\
         AddDrivers   ={add_drivers}\n\
         Drv_Source   =\\Drivers\n\
         \n\
         [Store_Apps]\n\
         SkipApps     =0\n\
         AppsLevel    =0\n\
         StubAppsFull =1\n\
         CustomList   =0\n\
         \n\
         [create_virtual_editions]\n\
         vUseDism     =1\n\
         vAutoStart   ={start_virtual}\n\
         vDeleteSource=0\n\
         vPreserve    =0\n\
         vwim2esd     ={wim2esd}\n\
         vwim2swm     =0\n\
         vSkipISO     =0\n\
         vAutoEditions=\n\
         vSortEditions=\n"
    )
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(md) = e.metadata() {
                total += md.len();
            }
        }
    }
    total
}

fn find_first_iso(dir: &Path) -> Option<PathBuf> {
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_file()
            && p.extension()
                .map(|x| x.eq_ignore_ascii_case("iso"))
                .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

fn which(prog: &str) -> Option<String> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let cand = dir.join(prog);
        if cand.is_file() {
            return Some(cand.to_string_lossy().into_owned());
        }
        #[cfg(target_os = "windows")]
        {
            let cand = dir.join(format!("{prog}.exe"));
            if cand.is_file() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} o")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

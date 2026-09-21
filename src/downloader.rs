//! Téléchargeur natif parallèle avec reprise (HTTP Range), vérification SHA-1 et annulation.
//!
//! Remplace aria2 dans le flux officiel : les fichiers UUP sont placés dans `UUPs/`,
//! exactement comme le package d'origine s'y attend pour la conversion.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Un fichier à télécharger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadItem {
    pub name: String,
    pub url: String,
    pub size: u64,
    #[serde(default)]
    pub sha1: String,
}

/// Événements envoyés au thread UI pendant le téléchargement.
#[derive(Debug, Clone)]
pub enum DlEvent {
    /// Un fichier change d'état (index dans la liste).
    State(usize, FileState),
    /// Progression d'un fichier : (index, octets nouvellement écrits).
    Progress(usize, u64),
    /// Message de journal.
    Log(String),
    /// Terminé : (succès, fichiers échoués).
    Finished(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileState {
    Pending,
    Downloading,
    Done,
    Failed(String),
}

/// Agent HTTP dédié aux gros fichiers : timeout de connexion uniquement.
fn dl_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .user_agent("uupdump-client-rs/0.1 (downloader natif)")
        .build()
}

/// Lance le téléchargement dans un thread dédié.
///
/// * `items` : fichiers (déjà fusionnés / dédoublonnés)
/// * `dest` : dossier cible (typiquement `<projet>/UUPs`)
/// * `threads` : téléchargements simultanés
/// * `verify_sha1` : vérifie l'empreinte après écriture (si sha1 connu)
pub fn spawn(
    items: Vec<DownloadItem>,
    dest: PathBuf,
    threads: usize,
    verify_sha1: bool,
    tx: std::sync::mpsc::Sender<DlEvent>,
    cancel: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("uupdl-coordinator".into())
        .spawn(move || {
            let _ = std::fs::create_dir_all(&dest);
            let queue: VecDeque<usize> = (0..items.len()).collect();
            let queue = Arc::new(Mutex::new(queue));
            let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let written_total = Arc::new(AtomicU64::new(0));

            let n_threads = threads.clamp(1, 12).min(items.len().max(1));
            let mut handles = Vec::new();
            for t in 0..n_threads {
                let queue = Arc::clone(&queue);
                let failures = Arc::clone(&failures);
                let written = Arc::clone(&written_total);
                let tx = tx.clone();
                let cancel = Arc::clone(&cancel);
                let dest = dest.clone();
                let items = items.clone();
                handles.push(
                    std::thread::Builder::new()
                        .name(format!("uupdl-worker-{t}"))
                        .spawn(move || {
                            loop {
                                if cancel.load(Ordering::Relaxed) {
                                    return;
                                }
                                let next = queue.lock().ok().and_then(|mut q| q.pop_front());
                                let Some(idx) = next else { return };
                                let item = &items[idx];
                                let _ = tx.send(DlEvent::State(idx, FileState::Downloading));
                                match download_one(item, &dest, &cancel, &written, &tx, idx) {
                                    Ok(path) => {
                                        if verify_sha1 && !item.sha1.is_empty() {
                                            match crate::util::sha1_file(&path) {
                                                Ok(h) if h.eq_ignore_ascii_case(&item.sha1) => {}
                                                Ok(h) => {
                                                    let msg = format!(
                                                        "{} : empreinte invalide (attendu {}, obtenu {})",
                                                        item.name, &item.sha1[..8.min(item.sha1.len())], &h[..8.min(h.len())]
                                                    );
                                                    let _ = tx.send(DlEvent::Log(format!("⚠ {msg}")));
                                                    failures.lock().ok().map(|mut f| f.push(msg.clone()));
                                                    let _ = tx.send(DlEvent::State(idx, FileState::Failed(msg)));
                                                    continue;
                                                }
                                                Err(e) => {
                                                    let _ = tx.send(DlEvent::Log(format!(
                                                        "⚠ Impossible de vérifier {} : {e}",
                                                        item.name
                                                    )));
                                                }
                                            }
                                        }
                                        let _ = tx.send(DlEvent::State(idx, FileState::Done));
                                    }
                                    Err(e) => {
                                        let msg = format!("{} : {e}", item.name);
                                        let _ = tx.send(DlEvent::Log(format!("✗ {msg}")));
                                        failures.lock().ok().map(|mut f| f.push(msg.clone()));
                                        let _ = tx.send(DlEvent::State(idx, FileState::Failed(msg)));
                                    }
                                }
                            }
                        })
                        .expect("spawn worker"),
                );
            }
            for h in handles {
                let _ = h.join();
            }

            let failed = failures.lock().map(|f| f.clone()).unwrap_or_default();
            let _ = tx.send(DlEvent::Finished(failed));
        })
        .expect("spawn coordinator")
}

/// Télécharge un fichier avec reprise et 3 tentatives.
fn download_one(
    item: &DownloadItem,
    dest: &std::path::Path,
    cancel: &AtomicBool,
    written_total: &AtomicU64,
    tx: &std::sync::mpsc::Sender<DlEvent>,
    idx: usize,
) -> Result<PathBuf, String> {
    let final_path = dest.join(&item.name);
    let part_path = dest.join(format!("{}.part", item.name));

    // Déjà complet d'une session précédente ?
    if let Ok(md) = std::fs::metadata(&final_path) {
        if item.size == 0 || md.len() == item.size {
            let _ = tx.send(DlEvent::State(idx, FileState::Done));
            return Ok(final_path);
        }
    }

    let mut last_err = String::from("inconnu");
    for attempt in 1..=3 {
        if cancel.load(Ordering::Relaxed) {
            return Err("annulé".into());
        }
        if attempt > 1 {
            std::thread::sleep(Duration::from_secs(2 * attempt as u64));
        }

        let existing = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        let res = (|| -> Result<(), String> {
            let agent = dl_agent();
            let mut req = agent.get(&item.url);
            if existing > 0 {
                req = req.set("Range", &format!("bytes={existing}-"));
            }
            let resp = req.call().map_err(|e| format!("requête : {e}"))?;

            let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
            // Si le serveur ignore la reprise, on repart de zéro.
            let resumed = existing > 0 && total.map(|t| t + existing != item.size || item.size == 0).unwrap_or(false);
            let append = existing > 0 && !resumed;
            if !append && existing > 0 {
                let _ = std::fs::remove_file(&part_path);
            }

            let mut out = std::fs::OpenOptions::new()
                .create(true)
                .append(append)
                .write(true)
                .open(&part_path)
                .map_err(|e| format!("fichier .part : {e}"))?;

            let mut reader = resp.into_reader();
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("annulé".into());
                }
                let n = reader.read(&mut buf).map_err(|e| format!("lecture : {e}"))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).map_err(|e| format!("écriture : {e}"))?;
                written_total.fetch_add(n as u64, Ordering::Relaxed);
                let _ = tx.send(DlEvent::Progress(idx, n as u64));
            }

            // Validation de taille (si connue).
            let have = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
            if item.size > 0 && have < item.size {
                return Err(format!("taille incomplète ({}/{})", have, item.size));
            }
            std::fs::rename(&part_path, &final_path).map_err(|e| format!("renommer : {e}"))?;
            Ok(())
        })();

        match res {
            Ok(()) => return Ok(final_path),
            Err(e) => {
                if e == "annulé" || cancel.load(Ordering::Relaxed) {
                    return Err("annulé".into());
                }
                last_err = e;
            }
        }
    }
    Err(last_err)
}

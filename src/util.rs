//! Petits utilitaires : formatage, ouverture du navigateur de fichiers, extraction.

use std::io::Read;
use std::path::Path;

/// Formate un nombre d'octets en chaîne lisible (Ko/Mo/Go).
pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} o")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Formate une vitesse en octets/s.
pub fn fmt_speed(bps: f64) -> String {
    if bps <= 0.0 {
        "—".into()
    } else {
        format!("{}/s", fmt_size(bps as u64))
    }
}

/// Formate un timestamp UNIX en date courte (YYYY-MM-DD HH:MM).
pub fn fmt_date(ts: i64) -> String {
    // Conversion civile sans dépendance externe (algorithme jours -> date).
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (h, mi, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Ouvre un dossier dans l'explorateur système.
pub fn open_folder(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Extrait une archive .zip vers un dossier.
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("Ouverture du zip : {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("Lecture du zip : {e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("Création du dossier : {e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("Entrée zip : {e}"))?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| format!("Dossier zip : {e}"))?;
        } else {
            if let Some(p) = out_path.parent() {
                std::fs::create_dir_all(p).map_err(|e| format!("Dossier parent : {e}"))?;
            }
            let mut out = std::fs::File::create(&out_path).map_err(|e| format!("Création fichier : {e}"))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| format!("Extraction : {e}"))?;
        }
    }
    Ok(())
}

/// Extrait une archive .7z vers un dossier (used en repli hors Windows).
pub fn extract_7z(archive: &Path, dest: &Path) -> Result<(), String> {
    sevenz_rust::decompress_file(archive, dest).map_err(|e| format!("Extraction 7z : {e}"))
}

/// Extrait une archive .7z en excluant certains noms de fichiers à la racine
/// (fidèle au comportement du script officiel qui préserve ConvertConfig.ini).
pub fn extract_7z_excluding_root(archive: &Path, dest: &Path, exclude_root: &[&str]) -> Result<(), String> {
    // sevenz-rust n'expose pas de filtre par entrée : extraction complète dans un
    // dossier temporaire, puis déplacement en respectant les exclusions.
    let tmp = dest.join("__conv_tmp__");
    extract_7z(archive, &tmp)?;
    let mut moved = 0usize;
    if let Ok(rd) = std::fs::read_dir(&tmp) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let target = dest.join(&name);
            if exclude_root.contains(&name.as_str()) {
                // supprimé pour préserver notre version patchée
                let _ = if e.path().is_dir() {
                    std::fs::remove_dir_all(e.path())
                } else {
                    std::fs::remove_file(e.path())
                };
                continue;
            }
            let _ = std::fs::rename(e.path(), &target).or_else(|_| {
                copy_recursive(&e.path(), &target).map(|_| ())
            });
            moved += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if moved == 0 {
        return Err("L'archive du convertisseur semble vide".into());
    }
    Ok(())
}

/// Copie récursive d'un dossier (utilisée pour centraliser les drivers).
pub fn copy_dir(src: &Path, dst: &Path) -> Result<u64, String> {
    if src.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("création {} : {e}", dst.display()))?;
        let mut n = 0;
        let rd = std::fs::read_dir(src).map_err(|e| format!("lecture {} : {e}", src.display()))?;
        for e in rd.flatten() {
            n += copy_recursive(&e.path(), &dst.join(e.file_name()))
                .map_err(|e| format!("copie : {e}"))?;
        }
        Ok(n)
    } else {
        copy_recursive(src, dst).map_err(|e| format!("copie : {e}"))
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<u64> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        let mut n = 0;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            n += copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(n)
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)
    }
}

/// Calcule le SHA-1 d'un fichier par blocs de 1 Mo.
pub fn sha1_file(path: &Path) -> Result<String, String> {
    use sha1::{Digest, Sha1};
    let mut f = std::fs::File::open(path).map_err(|e| format!("Ouverture : {e}"))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("Lecture : {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// SUPPRIME récursivement un dossier si le toggle auto-clean est actif.
pub fn clean_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|e| format!("Nettoyage de {} : {e}", path.display()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_size_basics() {
        assert_eq!(fmt_size(0), "0 o");
        assert_eq!(fmt_size(512), "512 o");
        assert_eq!(fmt_size(1024), "1.0 Ko");
        assert_eq!(fmt_size(4_080_000_000u64), "3.8 Go");
    }

    #[test]
    fn fmt_date_epoch() {
        assert_eq!(fmt_date(0), "1970-01-01 00:00:00");
        assert_eq!(fmt_date(86_400), "1970-01-02 00:00:00");
    }
}

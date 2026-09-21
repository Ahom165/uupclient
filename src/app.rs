//! Machine à états de l'application : écrans, opérations de fond (API, téléchargement,
//! création d'ISO) et leur coordination via canaux mpsc.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::api::ApiClient;
use crate::builder::{self, BuildContext, BuildEvent};
use crate::config::Settings;
use crate::downloader::{self, DlEvent, DownloadItem, FileState};
use crate::models::{Build, DlMode, EditionEntry, LangEntry};
use crate::util;

/// Écran courant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Screen {
    Search,
    Detail,
    Downloads,
    Finished,
}

/// Résultat des opérations API de fond.
pub enum OpEvent {
    SearchDone(Result<Vec<Build>, String>),
    LangsDone(Result<Vec<LangEntry>, String>),
    EditionsDone(Result<Vec<EditionEntry>, String>),
    FilesReady(Result<PreparedPlan, String>),
}

/// Plan préparé par le thread API juste avant le téléchargement.
pub struct PreparedPlan {
    pub slug: String,
    pub title: String,
    pub items: Vec<DownloadItem>,
    pub total_bytes: u64,
}

pub struct App {
    pub settings: Settings,
    pub screen: Screen,
    pub show_settings: bool,

    // Recherche
    pub query: String,
    pub results: Vec<Build>,
    pub selected: Option<usize>,
    pub busy: Option<String>,
    pub error: Option<String>,

    // Détail de build
    pub langs: Option<Vec<LangEntry>>,
    pub lang_idx: usize,
    pub editions: Vec<(EditionEntry, bool)>,
    pub mode: DlMode,

    // Téléchargement
    pub dl: DlState,
    // Création
    pub build: BuildState,

    // Journal global
    pub logs: VecDeque<String>,

    // Infra
    api: ApiClient,
    op_tx: mpsc::Sender<OpEvent>,
    op_rx: mpsc::Receiver<OpEvent>,
    pub dl_tx: mpsc::Sender<DlEvent>,
    dl_rx: mpsc::Receiver<DlEvent>,
    pub build_tx: mpsc::Sender<BuildEvent>,
    build_rx: mpsc::Receiver<BuildEvent>,
    pub cancel_dl: std::sync::Arc<AtomicBool>,
    pub cancel_build: std::sync::Arc<AtomicBool>,

    // Dernier plan (pour reprise)
    last_plan: Option<(PreparedPlan, PathBuf)>,
}

/// État du téléchargement en cours.
pub struct DlState {
    pub active: bool,
    pub cancelled: bool,
    pub finished: bool,
    pub title: String,
    pub items: Vec<DownloadItem>,
    pub states: Vec<FileState>,
    pub done: Vec<u64>,
    pub total_done: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub failed: Vec<String>,
    last_sample: (u64, Instant),
}

impl DlState {
    fn new() -> Self {
        Self {
            active: false,
            cancelled: false,
            finished: false,
            title: String::new(),
            items: Vec::new(),
            states: Vec::new(),
            done: Vec::new(),
            total_done: 0,
            total_bytes: 0,
            speed: 0.0,
            failed: Vec::new(),
            last_sample: (0, Instant::now()),
        }
    }

    pub fn files_done(&self) -> usize {
        self.states
            .iter()
            .filter(|s| matches!(s, FileState::Done))
            .count()
    }

    pub fn progress(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.total_done as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// État de la création d'ISO.
pub struct BuildState {
    pub active: bool,
    pub stage: String,
    pub logs: Vec<String>,
    pub iso: Option<PathBuf>,
    pub error: Option<String>,
}

impl BuildState {
    fn new() -> Self {
        Self {
            active: false,
            stage: String::new(),
            logs: Vec::new(),
            iso: None,
            error: None,
        }
    }
}

impl App {
    pub fn new() -> Self {
        let settings = Settings::load();
        let (op_tx, op_rx) = mpsc::channel();
        let (dl_tx, dl_rx) = mpsc::channel();
        let (build_tx, build_rx) = mpsc::channel();
        let mut logs = VecDeque::new();
        logs.push_back("Bienvenue — recherche une build, choisis langue/éditions, puis télécharge et crée l'ISO.".into());
        Self {
            settings,
            screen: Screen::Search,
            show_settings: false,
            query: String::new(),
            results: Vec::new(),
            selected: None,
            busy: None,
            error: None,
            langs: None,
            lang_idx: 0,
            editions: Vec::new(),
            mode: DlMode::Full,
            dl: DlState::new(),
            build: BuildState::new(),
            logs,
            api: ApiClient::new(),
            op_tx,
            op_rx,
            dl_tx,
            dl_rx,
            build_tx,
            build_rx,
            cancel_dl: std::sync::Arc::new(AtomicBool::new(false)),
            cancel_build: std::sync::Arc::new(AtomicBool::new(false)),
            last_plan: None,
        }
    }

    fn push_log(&mut self, line: impl Into<String>) {
        self.logs.push_back(line.into());
        while self.logs.len() > 400 {
            self.logs.pop_front();
        }
    }

    /// Dossier racine du projet (destination).
    pub fn project_root(&self) -> Option<PathBuf> {
        self.settings
            .dest_dir
            .as_ref()
            .map(PathBuf::from)
    }

    // ---------- Opérations API de fond ----------

    pub fn search(&mut self) {
        self.error = None;
        self.results.clear();
        self.selected = None;
        self.busy = Some("Recherche des builds…".into());
        let api = self.api.clone();
        let tx = self.op_tx.clone();
        let q = self.query.trim().to_string();
        let arch = self.settings.arch.clone();
        let ring = self.settings.channel.clone();
        std::thread::spawn(move || {
            let res = if q.is_empty() {
                api.fetch_channel(&arch, &ring)
            } else {
                api.search(&q)
            };
            let _ = tx.send(OpEvent::SearchDone(res.map_err(|e| e.to_string())));
        });
    }

    pub fn open_build(&mut self, idx: usize) {
        if idx >= self.results.len() {
            return;
        }
        self.selected = Some(idx);
        self.screen = Screen::Detail;
        self.langs = None;
        self.lang_idx = 0;
        self.editions.clear();
        self.error = None;
        let build = self.results[idx].clone();
        self.busy = Some("Chargement des langues…".into());
        let api = self.api.clone();
        let tx = self.op_tx.clone();
        let uuid = build.uuid.clone();
        std::thread::spawn(move || {
            let _ = tx.send(OpEvent::LangsDone(
                api.list_langs(&uuid).map_err(|e| e.to_string()),
            ));
        });
    }

    pub fn lang_chosen(&mut self, idx: usize) {
        self.lang_idx = idx;
        let Some(build) = self.selected_build() else { return };
        let Some(langs) = self.langs.clone() else { return };
        let Some(lang) = langs.get(idx) else { return };
        self.editions.clear();
        self.busy = Some("Chargement des éditions…".into());
        self.error = None;
        let api = self.api.clone();
        let tx = self.op_tx.clone();
        let uuid = build.uuid.clone();
        let lang = lang.code.clone();
        std::thread::spawn(move || {
            let _ = tx.send(OpEvent::EditionsDone(
                api.list_editions(&uuid, &lang).map_err(|e| e.to_string()),
            ));
        });
    }

    fn selected_build(&self) -> Option<Build> {
        self.selected.and_then(|i| self.results.get(i).cloned())
    }

    /// Variante publique pour l'UI.
    pub fn current_build(&self) -> Option<Build> {
        self.selected_build()
    }

    /// Lance la préparation (liste de fichiers) puis le téléchargement.
    pub fn start_download(&mut self) {
        self.error = None;
        if self.project_root().is_none() {
            self.error = Some("Choisis d'abord un dossier de destination dans les paramètres.".into());
            return;
        }
        let Some(build) = self.selected_build() else { return };

        let (lang, editions) = match self.mode {
            DlMode::Full => {
                let Some(langs) = &self.langs else { return };
                let Some(lang) = langs.get(self.lang_idx) else { return };
                let eds: Vec<String> = self
                    .editions
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(e, _)| e.key.clone())
                    .collect();
                if eds.is_empty() {
                    self.error = Some("Coche au moins une édition.".into());
                    return;
                }
                (Some(lang.code.clone()), eds)
            }
            DlMode::UpdatesOnly => (None, Vec::new()),
        };

        self.busy = Some("Récupération de la liste de fichiers…".into());
        let api = self.api.clone();
        let tx = self.op_tx.clone();
        let uuid = build.uuid.clone();
        let want_apps = self.settings.options.store_apps && self.mode == DlMode::Full;
        let mode = self.mode;
        let build_meta = build.clone();
        std::thread::spawn(move || {
            let res = (|| -> Result<PreparedPlan, String> {
                let (title, files) = api
                    .get_files(&uuid, lang.as_deref(), &editions)
                    .map_err(|e| e.to_string())?;
                let mut items: Vec<DownloadItem> = files
                    .into_iter()
                    .filter(|f| !f.url.is_empty())
                    .map(|f| DownloadItem {
                        name: f.name,
                        url: f.url,
                        size: f.size,
                        sha1: f.sha1,
                    })
                    .collect();
                if want_apps {
                    match api.get_files(&uuid, Some("neutral"), &["app".to_string()]) {
                        Ok((_, app_files)) => {
                            let known: std::collections::HashSet<String> =
                                items.iter().map(|i| i.name.clone()).collect();
                            for f in app_files {
                                if !f.url.is_empty() && !known.contains(&f.name) {
                                    items.push(DownloadItem {
                                        name: f.name,
                                        url: f.url,
                                        size: f.size,
                                        sha1: f.sha1,
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            // Les apps du Store sont optionnelles : non bloquant.
                            eprintln!("Apps du Store indisponibles (ignoré) : {e}");
                        }
                    }
                }
                let total: u64 = items.iter().map(|i| i.size).sum();
                let lang_part = lang.clone().unwrap_or_else(|| "updates".into());
                let ed_part = editions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| if editions.len() > 1 { "multi".into() } else { "all".into() });
                let slug = sanitize(&format!(
                    "{}_{}_{}_{}",
                    build_meta.build, build_meta.arch, lang_part, ed_part
                ));
                Ok(PreparedPlan {
                    slug,
                    title,
                    items,
                    total_bytes: total,
                })
            })();
            let _ = tx.send(OpEvent::FilesReady(res));
        });
        let _ = mode;
    }

    /// Démarre (ou reprend) effectivement le téléchargement des fichiers.
    fn begin_download(&mut self, plan: PreparedPlan) {
        let root = self.project_root().unwrap();
        let project = root.join(&plan.slug);
        let uups = project.join("UUPs");
        let _ = std::fs::create_dir_all(&uups);

        self.dl = DlState::new();
        self.dl.active = true;
        self.dl.cancelled = false;
        self.dl.finished = false;
        self.dl.title = plan.title.clone();
        self.dl.items = plan.items.clone();
        self.dl.states = vec![FileState::Pending; plan.items.len()];
        self.dl.done = vec![0; plan.items.len()];
        self.dl.total_bytes = plan.total_bytes;
        self.dl.total_done = 0;
        self.screen = Screen::Downloads;
        self.push_log(format!(
            "Téléchargement de {} fichiers ({}) vers {}",
            plan.items.len(),
            util::fmt_size(plan.total_bytes),
            uups.display()
        ));

        self.cancel_dl = std::sync::Arc::new(AtomicBool::new(false));
        let items = plan.items.clone();
        downloader::spawn(
            items,
            uups,
            self.settings.options.threads,
            self.settings.options.verify_hashes,
            self.dl_tx.clone(),
            std::sync::Arc::clone(&self.cancel_dl),
        );
        self.last_plan = Some((plan, project));
    }

    /// Reprend un téléchargement interrompu (les fichiers complets sont ignorés,
    /// les partiels reprennent via HTTP Range).
    pub fn resume_download(&mut self) {
        if let Some((plan, _)) = self.last_plan.take() {
            self.push_log("Reprise du téléchargement…");
            self.begin_download(plan);
        }
    }

    /// Copie du dernier plan (pour boutons / ouverture de dossier).
    pub fn last_plan_clone(&self) -> Option<(PreparedPlan, PathBuf)> {
        self.last_plan.as_ref().map(|(p, dir)| {
            let plan = PreparedPlan {
                slug: p.slug.clone(),
                title: p.title.clone(),
                items: p.items.clone(),
                total_bytes: p.total_bytes,
            };
            (plan, dir.clone())
        })
    }

    /// Réinitialise pour une nouvelle recherche (les réglages sont conservés).
    pub fn reset_for_new_search(&mut self) {
        self.screen = Screen::Search;
        self.selected = None;
        self.langs = None;
        self.lang_idx = 0;
        self.editions.clear();
        self.error = None;
        self.build = BuildState::new();
        self.dl = DlState::new();
        self.last_plan = None;
    }

    /// Démarre la création de l'ISO.
    pub fn start_build(&mut self) {
        let Some((_, project)) = self.last_plan_clone() else { return };
        let Some(build) = self.selected_build() else { return };
        let langs = self.langs.clone();
        let lang_code = match self.mode {
            DlMode::Full => langs
                .and_then(|l| l.get(self.lang_idx).map(|x| x.code.clone()))
                .unwrap_or_default(),
            DlMode::UpdatesOnly => return, // pas d'ISO pour les updates seules
        };
        let editions: Vec<String> = self
            .editions
            .iter()
            .filter(|(_, on)| *on)
            .map(|(e, _)| e.key.clone())
            .collect();

        self.build = BuildState::new();
        self.build.active = true;
        self.screen = Screen::Finished;
        self.push_log("Démarrage du pipeline de création d'ISO (package officiel UUP dump).");

        self.cancel_build = std::sync::Arc::new(AtomicBool::new(false));
        builder::spawn(
            BuildContext {
                update_id: build.uuid.clone(),
                lang: lang_code,
                editions,
            },
            self.settings.options.clone(),
            project,
            self.settings.drivers.clone(),
            self.build_tx.clone(),
            std::sync::Arc::clone(&self.cancel_build),
        );
    }

    /// Annule le téléchargement en cours.
    pub fn cancel_download(&mut self) {
        self.cancel_dl.store(true, Ordering::Relaxed);
        self.dl.cancelled = true;
        self.push_log("Annulation du téléchargement demandée…");
    }

    /// Retour à l'écran de recherche en conservant les réglages.
    pub fn back_to_search(&mut self) {
        self.screen = Screen::Search;
        self.error = None;
    }

    /// Sauvegarde des réglages (appelé sur chaque modification notable).
    pub fn save_settings(&mut self) {
        self.settings.save();
    }

    // ---------- Boucle d'événements ----------

    pub fn poll(&mut self, ctx: &egui::Context) {
        // Opérations API
        while let Ok(ev) = self.op_rx.try_recv() {
            match ev {
                OpEvent::SearchDone(res) => {
                    self.busy = None;
                    match res {
                        Ok(v) => {
                            if v.is_empty() {
                                self.error = Some("Aucune build trouvée pour cette recherche.".into());
                            }
                            self.results = v;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                OpEvent::LangsDone(res) => {
                    self.busy = None;
                    match res {
                        Ok(v) => {
                            // langue française en premier si présente
                            let mut v = v;
                            if let Some(pos) = v.iter().position(|l| l.code == "fr-fr") {
                                let item = v.remove(pos);
                                v.insert(0, item);
                                self.lang_idx = 0;
                            }
                            self.langs = Some(v);
                        }
                        Err(e) => {
                            self.error = Some(e);
                            self.screen = Screen::Search;
                        }
                    }
                }
                OpEvent::EditionsDone(res) => {
                    self.busy = None;
                    match res {
                        Ok(v) => {
                            // Pro cochée par défaut si présente
                            self.editions = v
                                .into_iter()
                                .map(|e| {
                                    let on = e.key.eq_ignore_ascii_case("professional");
                                    (e, on)
                                })
                                .collect();
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                OpEvent::FilesReady(res) => {
                    self.busy = None;
                    match res {
                        Ok(plan) => {
                            if plan.items.is_empty() {
                                self.error =
                                    Some("Aucun fichier téléchargeable pour cette sélection.".into());
                            } else {
                                self.begin_download(plan);
                            }
                        }
                        Err(e) => {
                            self.error = Some(e);
                            self.screen = Screen::Detail;
                        }
                    }
                }
            }
        }

        // Téléchargement
        while let Ok(ev) = self.dl_rx.try_recv() {
            match ev {
                DlEvent::State(i, s) => {
                    if let Some(st) = self.dl.states.get_mut(i) {
                        // Passage en Failed : comptabilise ce qui manque pour ce fichier
                        if matches!(s, FileState::Failed(_))
                            && !matches!(st, FileState::Failed(_) | FileState::Done)
                        {
                            let missing = self.dl.items[i].size.saturating_sub(self.dl.done[i]);
                            self.dl.total_bytes = self.dl.total_bytes.saturating_sub(missing);
                        }
                        *st = s;
                    }
                }
                DlEvent::Progress(i, d) => {
                    self.dl.done[i] += d;
                    self.dl.total_done += d;
                }
                DlEvent::Log(s) => self.push_log(s),
                DlEvent::Finished(failed) => {
                    self.dl.active = false;
                    self.dl.finished = true;
                    self.dl.failed = failed.clone();
                    if self.dl.cancelled {
                        self.push_log(format!(
                            "Téléchargement annulé — {} fichiers complets conservés, la reprise est possible.",
                            self.dl.files_done()
                        ));
                    } else if failed.is_empty() {
                        self.push_log(format!(
                            "Téléchargement terminé : {} fichiers ({}).",
                            self.dl.items.len(),
                            util::fmt_size(self.dl.total_done)
                        ));
                        // Enchaînement automatique vers la création d'ISO si configurée.
                        if self.mode == DlMode::Full && self.settings.options.make_iso {
                            self.start_build();
                        }
                    } else {
                        self.push_log(format!(
                            "Téléchargement terminé avec {} échec(s). Utilise « Reprendre ».",
                            failed.len()
                        ));
                    }
                }
            }
        }
        // Vitesse
        let (last_b, last_t) = self.dl.last_sample;
        if last_t.elapsed() >= Duration::from_millis(700) {
            let dt = last_t.elapsed().as_secs_f64();
            if dt > 0.0 {
                self.dl.speed = (self.dl.total_done - last_b) as f64 / dt;
            }
            self.dl.last_sample = (self.dl.total_done, Instant::now());
        }

        // Création d'ISO
        while let Ok(ev) = self.build_rx.try_recv() {
            match ev {
                BuildEvent::Stage(s) => {
                    self.build.stage = s.clone();
                    self.push_log(format!("▶ {s}"));
                }
                BuildEvent::Log(s) => {
                    self.build.logs.push(s.clone());
                    self.push_log(s);
                }
                BuildEvent::IsoFound(p) => {
                    self.build.iso = Some(p.clone());
                    self.build.active = false;
                    self.build.stage = "ISO prête".into();
                    self.push_log(format!("✓ ISO prête : {}", p.display()));
                    self.settings.save();
                }
                BuildEvent::Failed(e) => {
                    self.build.active = false;
                    self.build.error = Some(e.clone());
                    self.push_log(format!("✗ {e}"));
                }
            }
        }

        // Repaint automatique pendant les activités de fond
        if self.busy.is_some() || self.dl.active || self.build.active {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// L'application eframe.
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        crate::ui::render(ctx, self);
    }
}

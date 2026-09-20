//! Interface egui — 4 étapes : Recherche → Package → Options → Téléchargement.

use crate::api::{self, BuildEntry, Editions, Langs, UpdateCandidate};
use crate::job::{self, JobRequest, JobState, Phase, SharedJob};
use crate::store::{self, Compression, Config, DriverEntry, DriverTarget};
use eframe::egui;
use egui::{Color32, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ état asynchrone

/// Événements renvoyés par les threads d'API vers l'UI.
enum Event {
    SearchDone(Result<Vec<BuildEntry>, String>),
    FetchDone(Result<Vec<UpdateCandidate>, String>),
    LangsDone(Result<Langs, String>),
    EditionsDone(Result<Editions, String>),
    EstimateDone(Result<(String, u64, usize), String>), // (nom, octets, n fichiers)
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Search,
    Package,
    Options,
    Download,
}

#[derive(PartialEq, Clone, Copy)]
enum SearchMode {
    /// Recherche dans la base des builds connues (comme la recherche du site).
    Known,
    /// Interroger Windows Update directement pour la toute dernière build.
    Wu,
    /// Coller un identifiant d'update (UUID).
    Direct,
}

#[derive(Clone, Copy, PartialEq)]
struct Channel {
    id: &'static str,
    label: &'static str,
}

const CHANNELS: [Channel; 5] = [
    Channel {
        id: "retail",
        label: "Retail (stable)",
    },
    Channel {
        id: "rp",
        label: "Release Preview",
    },
    Channel {
        id: "beta",
        label: "Beta",
    },
    Channel {
        id: "dev",
        label: "Dev",
    },
    Channel {
        id: "canary",
        label: "Canary",
    },
];

const ARCHES: [&str; 4] = ["amd64", "arm64", "x86", "all"];

pub struct App {
    cfg: Config,
    page: Page,

    // recherche
    mode: SearchMode,
    results: Vec<BuildEntry>,
    filter: String,
    selected: Option<BuildEntry>,
    direct_id: String,
    busy: bool,
    error: Option<String>,

    // package
    langs: Option<Langs>,
    editions: Option<Editions>,
    lang: String,
    selected_editions: Vec<String>,
    loading_meta: bool,
    estimate: Option<(String, u64, usize)>,
    estimate_busy: bool,

    // job
    job: SharedJob,
    cancel: Arc<AtomicBool>,
    job_running: bool,

    events: Arc<Mutex<std::sync::mpsc::Receiver<Event>>>,
    tx: std::sync::mpsc::Sender<Event>,

    dark: bool,
}

impl App {
    pub fn new() -> Self {
        let cfg = store::load();
        let (tx, rx) = std::sync::mpsc::channel();
        let job: SharedJob = Arc::new(Mutex::new(JobState::default()));
        Self {
            cfg,
            page: Page::Search,
            mode: SearchMode::Known,
            results: Vec::new(),
            filter: String::new(),
            selected: None,
            direct_id: String::new(),
            busy: false,
            error: None,
            langs: None,
            editions: None,
            lang: String::new(),
            selected_editions: Vec::new(),
            loading_meta: false,
            estimate: None,
            estimate_busy: false,
            job,
            cancel: Arc::new(AtomicBool::new(false)),
            job_running: false,
            events: Arc::new(Mutex::new(rx)),
            tx,
            dark: true,
        }
    }

    // ------------------------------------------------------------- threads API

    fn search(&mut self) {
        self.busy = true;
        self.error = None;
        let tx = self.tx.clone();
        match self.mode {
            SearchMode::Known => {
                let q = self.cfg.search_text.clone();
                std::thread::spawn(move || {
                    let r = api::list_ids(&q, true).map_err(|e| e.to_string());
                    let _ = tx.send(Event::SearchDone(r));
                });
            }
            SearchMode::Wu => {
                let (arch, ring, mut build) = (
                    self.cfg.arch.clone(),
                    self.cfg.channel.clone(),
                    self.cfg.search_text.trim().to_string(),
                );
                if build.is_empty() {
                    build = "latest".into();
                }
                std::thread::spawn(move || {
                    let r = api::fetch_upd(&arch, &ring, &build).map_err(|e| e.to_string());
                    let _ = tx.send(Event::FetchDone(r));
                });
            }
            SearchMode::Direct => {
                // Validé localement : passe directement à l'écran package.
                let id = self.direct_id.trim().to_string();
                if !valid_uuid(&id) {
                    self.error = Some("Identifiant invalide : collez un UUID du style 12345678-abcd-…(éventuellement suivi de _rev.2)".into());
                    self.busy = false;
                    return;
                }
                let cand = UpdateCandidate {
                    update_id: id,
                    title: "Update personnalisée (ID collé)".into(),
                    build: "—".into(),
                    arch: "—".into(),
                };
                self.selected = Some(BuildEntry {
                    title: cand.title.clone(),
                    build: cand.build.clone(),
                    arch: cand.arch.clone(),
                    created: 0,
                    uuid: cand.update_id.clone(),
                });
                self.busy = false;
                self.page = Page::Package;
                self.load_meta();
                return;
            }
        }
    }

    fn load_meta(&mut self) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        self.loading_meta = true;
        self.error = None;
        self.langs = None;
        self.editions = None;
        self.selected_editions.clear();
        self.estimate = None;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let langs = api::list_langs(&sel.uuid).map_err(|e| e.to_string());
            let _ = tx.send(Event::LangsDone(langs));
        });
    }

    fn load_editions(&mut self) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        let lang = self.lang.clone();
        if lang.is_empty() {
            return;
        }
        self.loading_meta = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let ed = api::list_editions(&lang, &sel.uuid).map_err(|e| e.to_string());
            let _ = tx.send(Event::EditionsDone(ed));
        });
    }

    fn estimate_size(&mut self) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        let lang = self.lang.clone();
        let edition = self
            .selected_editions
            .first()
            .cloned()
            .unwrap_or_else(|| "0".into());
        self.estimate_busy = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = api::get_files(&sel.uuid, &lang, &edition)
                .map(|p| {
                    (
                        p.update_name,
                        p.files.values().map(|f| f.size.max(0) as u64).sum::<u64>(),
                        p.files.len(),
                    )
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(Event::EstimateDone(r));
        });
    }

    fn start_job(&mut self) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        let req = JobRequest {
            update_id: sel.uuid.clone(),
            title: sel.title.clone(),
            build: sel.build.clone(),
            arch: sel.arch.clone(),
            lang: self.lang.clone(),
            lang_fancy: self
                .langs
                .as_ref()
                .and_then(|l| l.fancy.get(&self.lang).cloned())
                .unwrap_or_else(|| self.lang.clone()),
            edition: self
                .selected_editions
                .first()
                .cloned()
                .unwrap_or_else(|| "0".into()),
            edition_fancy: self
                .editions
                .as_ref()
                .and_then(|e| {
                    e.fancy.get(
                        self.selected_editions
                            .first()
                            .map(|s| s.as_str())
                            .unwrap_or(""),
                    )
                })
                .cloned()
                .unwrap_or_default(),
            options: self.cfg.options.clone(),
            drivers: self.cfg.drivers.clone(),
            output_dir: self.cfg.output_dir.clone(),
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        self.job_running = true;
        let st = self.job.clone();
        job::spawn(req, st, self.cancel.clone());
        self.page = Page::Download;
    }

    // ------------------------------------------------------------- helpers UI

    fn fancy_edition(&self, code: &str) -> String {
        self.editions
            .as_ref()
            .and_then(|e| e.fancy.get(code).cloned())
            .unwrap_or_else(|| code.to_string())
    }

    fn filtered_results(&self) -> Vec<&BuildEntry> {
        let f = self.filter.to_lowercase();
        self.results
            .iter()
            .filter(|b| {
                f.is_empty()
                    || b.title.to_lowercase().contains(&f)
                    || b.build.contains(&f)
                    || b.arch.contains(&f)
            })
            .take(200)
            .collect()
    }
}

/// Largeur responsive pour un champ dans une rangée horizontale : occupe
/// l'espace restant (en réservant `reserved` px pour le(s) widget(s) suivants),
/// borné entre `min` et `max`. Évite que les widgets fixent une largeur
/// constante et sortent de l'écran quand la fenêtre est étroite.
fn field_w(ui: &egui::Ui, reserved: f32, min: f32, max: f32) -> f32 {
    (ui.available_width() - reserved).clamp(min, max)
}

/// Tronque une chaîne à `max` caractères avec une ellipse (sécurisé UTF-8).
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Carte sobre : cadre arrondi, liseré discret, marge intérieure constante.
/// Toutes les pages l'utilisent pour un même rythme visuel.
fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    egui::Frame::new()
        .stroke(stroke)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            // Pleine largeur : toutes les cartes d'une même page ont des
            // bordures alignées (lecture verticale régulière).
            ui.set_min_width(ui.available_width() - 1.0);
            if !title.is_empty() {
                ui.label(RichText::new(title).strong().small());
                ui.add_space(2.0);
            }
            add(ui)
        })
        .inner
}

/// Bouton d'action principal : fond accent, texte blanc.
fn primary(ui: &mut egui::Ui, label: &str, accent: Color32, enabled: bool) -> bool {
    let btn = egui::Button::new(RichText::new(label).strong().color(Color32::WHITE))
        .fill(accent)
        .min_size(egui::vec2(180.0, 30.0));
    ui.add_enabled(enabled, btn).clicked()
}

/// Pied de page commun aux écrans de configuration : retour à gauche,
/// action principale à droite. Renvoie Some(true) = action principale,
/// Some(false) = retour, None = rien.
fn footer(
    ui: &mut egui::Ui,
    back_label: &str,
    next: Option<(&str, bool)>,
    accent: Color32,
) -> Option<bool> {
    ui.separator();
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button(back_label).clicked() {
            action = Some(false);
        }
        if let Some((label, enabled)) = next {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, label, accent, enabled) {
                    action = Some(true);
                }
            });
        }
    });
    action
}

fn valid_uuid(s: &str) -> bool {
    let base = s.split("_rev.").next().unwrap_or(s);
    let parts: Vec<&str> = base.split('-').collect();
    parts.len() == 8
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[7].len() == 12
        && base.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn human_size(bytes: u64) -> String {
    job::human_size(bytes)
}

/// Sélecteur de dossier natif sans dépendance : boîte de dialogue Windows Forms
/// via PowerShell sous Windows ; ailleurs, la saisie se fait par champ texte.
fn pick_folder() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; \
                      $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
                      if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }";
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", script])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Événements des threads
        loop {
            let ev = {
                let rx = self.events.lock().unwrap();
                rx.try_recv()
            };
            match ev {
                Ok(Event::SearchDone(Ok(mut v))) => {
                    v.sort_by(|a, b| b.created.cmp(&a.created));
                    self.results = v;
                    self.busy = false;
                    if self.results.is_empty() {
                        self.error = Some("Aucun résultat. Essayez un autre terme (ex. 26100) ou le mode « Dernières via WU ».".into());
                    }
                }
                Ok(Event::SearchDone(Err(e))) => {
                    self.busy = false;
                    self.error = Some(e);
                }
                Ok(Event::FetchDone(Ok(v))) => {
                    self.results = v
                        .into_iter()
                        .map(|u| BuildEntry {
                            title: u.title,
                            build: u.build,
                            arch: u.arch,
                            created: 0,
                            uuid: u.update_id,
                        })
                        .collect();
                    self.busy = false;
                    if self.results.is_empty() {
                        self.error = Some(
                            "Windows Update n'a rien renvoyé pour ce canal/numéro. Astuce : essayez le canal « Release Preview », un numéro de build précis (ex. 26100), ou la recherche dans les builds connues.".into(),
                        );
                    }
                }
                Ok(Event::FetchDone(Err(e))) => {
                    self.busy = false;
                    self.error = Some(e);
                }
                Ok(Event::LangsDone(Ok(l))) => {
                    self.langs = Some(l);
                    self.lang = if self
                        .langs
                        .as_ref()
                        .unwrap()
                        .list
                        .iter()
                        .any(|x| x == &self.cfg.last_lang)
                    {
                        self.cfg.last_lang.clone()
                    } else {
                        self.langs
                            .as_ref()
                            .unwrap()
                            .list
                            .first()
                            .cloned()
                            .unwrap_or_default()
                    };
                    self.loading_meta = false;
                    // Liste vide (update sans langues) : ne pas relancer la
                    // cascade d'éditions, sinon l'écran reste bloqué.
                    if !self.lang.is_empty() {
                        self.load_editions();
                    }
                }
                Ok(Event::LangsDone(Err(e))) => {
                    self.loading_meta = false;
                    self.error = Some(e);
                }
                Ok(Event::EditionsDone(Ok(e))) => {
                    self.editions = Some(e);
                    self.loading_meta = false;
                    // Pré-sélection : Professional sinon Home sinon première.
                    let list = &self.editions.as_ref().unwrap().list;
                    let pick = ["PROFESSIONAL", "CORE", "CLOUD"]
                        .iter()
                        .find_map(|p| list.iter().find(|x| x.contains(p)))
                        .or_else(|| list.first());
                    if let Some(p) = pick {
                        self.selected_editions = vec![p.to_string()];
                    }
                }
                Ok(Event::EditionsDone(Err(e))) => {
                    self.loading_meta = false;
                    self.error = Some(e);
                }
                Ok(Event::EstimateDone(Ok(v))) => {
                    self.estimate = Some(v);
                    self.estimate_busy = false;
                }
                Ok(Event::EstimateDone(Err(e))) => {
                    self.error = Some(e);
                    self.estimate_busy = false;
                }
                Err(_) => break,
            }
        }

        if self
            .error
            .as_deref()
            .map(|e| e.contains("Trop de requêtes"))
            .unwrap_or(false)
        {
            // L'API limite le débit : le message s'affiche à l'utilisateur ; pas de réessai automatique.
        }

        // Persistance légère
        store::save(&self.cfg);

        // Repaint périodique pendant un job
        if self.job_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(400));
        }

        self.draw(ctx);
    }
}

impl App {
    fn draw(&mut self, ctx: &egui::Context) {
        let accent = if self.dark {
            Color32::from_rgb(96, 148, 214)
        } else {
            Color32::from_rgb(47, 94, 158)
        };

        // Thème
        let visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        ctx.set_visuals(visuals);
        ctx.style_mut(|s| {
            s.visuals.widgets.hovered.fg_stroke.color = accent;
            // NB : ne PAS toucher à widgets.active.fg_stroke — c'est la couleur
            // du texte « strong », la forcer à l'accent rendait le texte
            // invisible sur les pilules sélectionnées (bleu sur bleu).
            s.visuals.selection.bg_fill = accent;
            // Texte lisible sur toute sélection (blanc sur bleu).
            s.visuals.selection.stroke.color = Color32::WHITE;
            s.visuals.hyperlink_color = accent;
            // Rythme vertical homogène + boutons plus respirants.
            s.spacing.item_spacing = egui::vec2(8.0, 8.0);
            s.spacing.button_padding = egui::vec2(10.0, 4.0);
        });

        self.header(ctx, accent);
        // Barre de recherche FIXE en haut (page Builds) : dessinée dans un
        // panneau dédié (TopBottomPanel), elle ne peut structurellement JAMAIS
        // défiler ni sortir de l'écran — quelles que soient la taille de la
        // fenêtre ou la longueur des résultats. Seule la liste des résultats,
        // dans le panneau central en dessous, défile.
        if self.page == Page::Search {
            egui::TopBottomPanel::top("search_bar")
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)))
                .show(ctx, |ui| self.search_controls(ui));
        }
        self.bottom_bar(ctx, accent);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)))
            .show(ctx, |ui| match self.page {
                Page::Search => self.page_results(ui),
                // Défilement vertical de page : aucun élément n'est jamais coupé
                // en bas, quelle que soit la hauteur de la fenêtre.
                Page::Package => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.page_package(ui, accent))
                        .inner
                }
                Page::Options => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.page_options(ui, accent))
                        .inner
                }
                Page::Download => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.page_download(ui, accent))
                        .inner
                }
            });
    }

    /// En-tête unique : titre + étapes 1-4 cliquables (pilules) + bascule de
    /// thème, puis rappel discret de la sélection. Remplace l'ancienne barre
    /// de titre ET le rail latéral : un seul niveau de chrome, contenu plus large.
    fn header(&mut self, ctx: &egui::Context, accent: Color32) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("UUP dump Client")
                        .size(16.0)
                        .strong()
                        .color(accent),
                );
                ui.separator();
                let step = self.page;
                let has_sel = self.selected.is_some();
                let has_pkg = !self.lang.is_empty() && !self.selected_editions.is_empty();
                let items = [
                    (Page::Search, "1  Builds", true),
                    (Page::Package, "2  Package", has_sel),
                    (Page::Options, "3  Options", has_sel),
                    (
                        Page::Download,
                        "4  Téléchargement",
                        has_pkg || self.job_running,
                    ),
                ];
                for (p, label, enabled) in items {
                    let active = p == step;
                    // Blanc explicite sur la pilule active (fond accent).
                    let txt = if active {
                        RichText::new(label).strong().color(Color32::WHITE)
                    } else {
                        RichText::new(label)
                    };
                    let resp = ui.add_enabled(
                        enabled || active,
                        egui::Button::new(txt)
                            .fill(if active { accent } else { Color32::TRANSPARENT })
                            .min_size(egui::vec2(0.0, 26.0)),
                    );
                    if resp.clicked() && p != step {
                        self.page = p;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let icon = if self.dark { "☀" } else { "☾" };
                    if ui.small_button(icon).clicked() {
                        self.dark = !self.dark;
                    }
                });
            });
            if let Some(s) = &self.selected {
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Sélection :").weak().small());
                    ui.label(
                        RichText::new(format!("{} · {}", s.build, s.arch))
                            .strong()
                            .small(),
                    );
                    let maxc = ((ui.available_width() - 30.0) / 6.5).clamp(20.0, 110.0) as usize;
                    ui.label(RichText::new(ellipsize(&s.title, maxc)).weak().small());
                });
            }
            ui.add_space(6.0);
        });
    }

    fn bottom_bar(&mut self, ctx: &egui::Context, accent: Color32) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let st = self.job.lock().unwrap();
                match &st.phase {
                    Phase::Idle => {
                        ui.label(RichText::new("Prêt.").weak());
                    }
                    Phase::FetchingList => {
                        ui.label("Récupération de la liste…");
                    }
                    Phase::Preparing => {
                        ui.label("Préparation du convertisseur…");
                    }
                    Phase::Downloading => {
                        ui.label(format!(
                            "Téléchargement {}/{} — {}/{}",
                            st.files_done,
                            st.files_total,
                            human_size(st.bytes_done),
                            human_size(st.bytes_total)
                        ));
                        if st.speed_bps > 0 {
                            ui.label(
                                RichText::new(format!("({}/s)", human_size(st.speed_bps))).weak(),
                            );
                        }
                        let frac = if st.bytes_total > 0 {
                            (st.bytes_done as f32 / st.bytes_total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let bar = egui::ProgressBar::new(frac).show_percentage();
                        ui.add(bar);
                    }
                    Phase::Converting => {
                        ui.label(
                            "Conversion ISO en cours (fenêtre de conversion / journal ci-dessous)…",
                        );
                    }
                    Phase::CleaningUp => {
                        ui.label("Nettoyage…");
                    }
                    Phase::Done => {
                        ui.label(RichText::new("Terminé").color(accent).strong());
                    }
                    Phase::Failed(e) => {
                        ui.label(
                            RichText::new(format!("Échec : {e}"))
                                .color(Color32::from_rgb(224, 96, 96)),
                        );
                    }
                }
            });
            ui.add_space(4.0);
        });
    }

    // --------------------------------------------------------------- page 1

    /// Barre de recherche — dessinée dans un TopBottomPanel FIXE, au-dessus
    /// du panneau central : impossible à faire défiler, toujours visible en
    /// haut de la fenêtre. Tout est en horizontal_wrapped : se replie au lieu
    /// de déborder quand la fenêtre est étroite.
    fn search_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (m, label) in [
                (SearchMode::Known, "Builds connues"),
                (SearchMode::Wu, "Dernières via WU"),
                (SearchMode::Direct, "ID d'update"),
            ] {
                let sel = self.mode == m;
                // Blanc explicite sur la pilule sélectionnée (fond bleu).
                let txt = if sel {
                    RichText::new(label).strong().color(Color32::WHITE)
                } else {
                    RichText::new(label).strong()
                };
                if ui.selectable_label(sel, txt).clicked() {
                    self.mode = m;
                    self.error = None;
                }
                ui.add_space(6.0);
            }
        });
        ui.add_space(8.0);

        match self.mode {
            SearchMode::Known => {
                let resp = ui.horizontal_wrapped(|ui| {
                    // Libellés alignés sur une même largeur : les champs de la
                    // barre commencent tous au même endroit.
                    ui.add_sized([76.0, 18.0], egui::Label::new("Recherche :"));
                    // Largeur responsive : le champ occupe l'espace restant
                    // (réservé pour le bouton) au lieu d'une largeur fixe qui
                    // débordait hors de l'écran sur les fenêtres étroites.
                    let w = field_w(ui, 150.0, 140.0, 460.0);
                    let r = ui.add_sized(
                        [w, 24.0],
                        egui::TextEdit::singleline(&mut self.cfg.search_text)
                            .hint_text("ex. 26100, 22631, « Windows Server », vide = tout"),
                    );
                    let btn = ui.button(RichText::new("Rechercher").strong());
                    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) || btn.clicked()
                });
                if resp.inner {
                    self.search();
                }
                ui.add_space(4.0);
                ui.label(RichText::new(
                    "Interroge la base des builds connues d'UUP dump (comme la recherche du site). Laissez vide pour parcourir les dernières builds.",
                ).weak().small());
            }
            SearchMode::Wu => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_sized([76.0, 18.0], egui::Label::new("Canal :"));
                    for c in CHANNELS {
                        if ui
                            .selectable_label(self.cfg.channel == c.id, c.label)
                            .clicked()
                        {
                            self.cfg.channel = c.id.to_string();
                        }
                        ui.add_space(2.0);
                    }
                });
                ui.add_space(6.0);
                let resp = ui.horizontal_wrapped(|ui| {
                    ui.add_sized([76.0, 18.0], egui::Label::new("Build :"));
                    let w = field_w(ui, 150.0, 130.0, 260.0);
                    let r = ui.add_sized(
                        [w, 24.0],
                        egui::TextEdit::singleline(&mut self.cfg.search_text).hint_text(
                            "latest (Insider) — ex. 26100.5010 ; Retail : 19045, 26100…",
                        ),
                    );
                    let btn = ui.button(RichText::new("Rechercher").strong());
                    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) || btn.clicked()
                });
                if resp.inner {
                    self.search();
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.add_sized([76.0, 18.0], egui::Label::new("Arch :"));
                    for a in ARCHES {
                        if ui.selectable_label(self.cfg.arch == a, a).clicked() {
                            self.cfg.arch = a.to_string();
                        }
                        ui.add_space(2.0);
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new(
                    "Interroge les serveurs Windows Update en direct. Retourne souvent plusieurs variantes (cumulative, feature…) : choisissez la « Feature Update ».",
                ).weak().small());
            }
            SearchMode::Direct => {
                let resp = ui.horizontal_wrapped(|ui| {
                    ui.add_sized([76.0, 18.0], egui::Label::new("Update ID :"));
                    let w = field_w(ui, 150.0, 140.0, 460.0);
                    let r = ui.add_sized(
                        [w, 24.0],
                        egui::TextEdit::singleline(&mut self.direct_id)
                            .hint_text("ex. 27720ec5-c721-4278-8c33-2ba9b48f5ee9"),
                    );
                    let btn = ui.button(RichText::new("Utiliser").strong());
                    (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        || btn.clicked()
                });
                if let Some(f) = self.direct_id.find(|c: char| c == '\n') {
                    self.direct_id.truncate(f);
                }
                if resp.inner {
                    self.search();
                }
                ui.add_space(4.0);
                ui.label(RichText::new(
                    "Collez l'identifiant d'une update (UUID, éventuellement suivi de _rev.N) récupéré sur uupdump.net ou via l'API.",
                ).weak().small());
            }
        }

        ui.add_space(10.0);
        if self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Interrogation de l'API…");
            });
        }
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(Color32::from_rgb(224, 110, 110)));
        }
    }

    /// Résultats de recherche — panneau central, SOUS la barre fixe. Seule
    /// cette liste défile ; la barre de recherche reste plantée en haut.
    fn page_results(&mut self, ui: &mut egui::Ui) {
        if self.results.is_empty() {
            if !self.busy && self.error.is_none() {
                ui.add_space(4.0);
                ui.label(RichText::new(
                    "Lancez une recherche ci-dessus — ex. « 26100 » — puis cliquez sur une build pour la configurer.",
                ).weak());
            }
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{} résultat(s)", self.results.len())).strong());
            if self.mode == SearchMode::Known {
                ui.separator();
                ui.label("Filtre :");
                let w = field_w(ui, 30.0, 90.0, 220.0);
                ui.add_sized(
                    [w, 20.0],
                    egui::TextEdit::singleline(&mut self.filter).hint_text("filtrer…"),
                );
            }
        });
        ui.add_space(4.0);

        let mut chosen: Option<String> = None;
        let rows = self.filtered_results();
        let sel_uuid = self.selected.as_ref().map(|s| s.uuid.clone());
        ui.push_id("results", |ui| {
            // both() : une barre horizontale apparaît si la fenêtre est
            // vraiment étroite — rien ne sort jamais de l'écran.
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Longueur de titre adaptée à la largeur : les colonnes
                    // Build/Arch/Date (~230 px) restent visibles sans scroll.
                    let max_chars =
                        ((ui.available_width() - 230.0) / 7.3).clamp(24.0, 64.0) as usize;
                    egui::Grid::new("res")
                        .num_columns(4)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.label(RichText::new("Build").strong());
                            ui.label(RichText::new("Titre").strong());
                            ui.label(RichText::new("Arch").strong());
                            ui.label(RichText::new("Date").strong());
                            ui.end_row();
                            for b in &rows {
                                let selected = sel_uuid.as_deref() == Some(b.uuid.as_str());
                                // Toute la ligne est cliquable (avant : seuls
                                // les titres) — plus naturel à parcourir.
                                let build_lbl = egui::Label::new(
                                    RichText::new(&b.build).strong().color(if selected {
                                        Color32::from_rgb(96, 148, 214)
                                    } else {
                                        ui.visuals().text_color()
                                    }),
                                )
                                .sense(egui::Sense::click());
                                let b_resp = ui.add(build_lbl);
                                // Titre borné (ne pousse plus la grille hors
                                // de l'écran) ; le titre complet au survol.
                                let title =
                                    egui::Label::new(RichText::new(ellipsize(&b.title, max_chars)))
                                        .sense(egui::Sense::click());
                                let t_resp = ui.add(title).on_hover_text(b.title.as_str());
                                let a_resp = ui.add(
                                    egui::Label::new(RichText::new(&b.arch).weak())
                                        .sense(egui::Sense::click()),
                                );
                                let date = if b.created > 0 {
                                    chrono_days_ago(b.created)
                                } else {
                                    "—".into()
                                };
                                let d_resp = ui.add(
                                    egui::Label::new(RichText::new(date).weak())
                                        .sense(egui::Sense::click()),
                                );
                                ui.end_row();
                                if b_resp.clicked()
                                    || t_resp.clicked()
                                    || t_resp.double_clicked()
                                    || a_resp.clicked()
                                    || d_resp.clicked()
                                {
                                    chosen = Some(b.uuid.clone());
                                }
                            }
                        });
                });
        });
        if let Some(uuid) = chosen {
            if let Some(b) = self.results.iter().find(|b| b.uuid == uuid).cloned() {
                self.selected = Some(b);
                self.page = Page::Package;
                self.load_meta();
            }
        }
    }

    // --------------------------------------------------------------- page 2

    fn page_package(&mut self, ui: &mut egui::Ui, accent: Color32) {
        // Rappel discret : l'en-tête affiche déjà build · arch · titre.
        if let Some(s) = &self.selected {
            ui.label(RichText::new(format!("Update : {}", s.uuid)).weak().small());
        }

        if self.loading_meta {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Chargement des langues / éditions…");
            });
            return;
        }
        // Les erreurs API (ex. rate-limit) doivent être visibles ici aussi,
        // avec un moyen de relancer — sinon l'écran reste sur « Chargement… ».
        if let Some(e) = self.error.clone() {
            card(ui, "", |ui| {
                ui.label(RichText::new(e).color(Color32::from_rgb(224, 110, 110)));
                if ui.button("Réessayer").clicked() {
                    self.load_meta();
                }
            });
            ui.add_space(8.0);
        }

        // Langue
        card(ui, "Langue", |ui| {
            let no_langs = self
                .langs
                .as_ref()
                .map(|l| l.list.is_empty())
                .unwrap_or(false);
            if no_langs {
                ui.label(RichText::new(
                    "Aucune langue disponible pour cette update (packages .NET/arm64 par exemple). Choisissez une autre build — de préférence une « Feature Update ».",
                ).weak());
            } else if self.langs.is_some() {
                let lang_list: Vec<String> = self.langs.as_ref().unwrap().list.clone();
                let lang_fancy: std::collections::HashMap<String, String> =
                    self.langs.as_ref().unwrap().fancy.clone();
                let names: Vec<String> = lang_list
                    .iter()
                    .map(|c| {
                        format!(
                            "{} ({})",
                            lang_fancy.get(c).cloned().unwrap_or_else(|| c.clone()),
                            c
                        )
                    })
                    .collect();
                let mut idx = lang_list.iter().position(|c| *c == self.lang).unwrap_or(0);
                egui::ComboBox::from_id_salt("langsel")
                    .selected_text(names.get(idx).cloned().unwrap_or_default())
                    .width(field_w(ui, 24.0, 200.0, 340.0))
                    .show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut idx, i, n.clone());
                        }
                    });
                if idx < lang_list.len() && lang_list[idx] != self.lang {
                    self.lang = lang_list[idx].clone();
                    self.cfg.last_lang = self.lang.clone();
                    self.editions = None;
                    self.selected_editions.clear();
                    self.estimate = None;
                    self.load_editions();
                }
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Langues indisponibles.").weak());
                    if ui.button("Charger").clicked() {
                        self.load_meta();
                    }
                });
            }
        });
        ui.add_space(8.0);

        // Éditions
        card(ui, "Éditions", |ui| {
            if self.editions.is_some() {
                let eds_list: Vec<String> = self.editions.as_ref().unwrap().list.clone();
                let mut sorted: Vec<(String, String)> = eds_list
                    .iter()
                    .map(|c| (c.clone(), self.fancy_edition(c)))
                    .collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    // Hauteur adaptée au contenu (pas de grand vide) : seule
                    // la largeur occupe toute la carte ; max 240 px.
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("eds")
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                for (code, fancy) in &sorted {
                                    let mut on = self.selected_editions.contains(code);
                                    if ui.checkbox(&mut on, format!("{fancy}  [{code}]")).changed()
                                    {
                                        if on {
                                            if !self.selected_editions.contains(code) {
                                                self.selected_editions.push(code.clone());
                                            }
                                        } else {
                                            self.selected_editions.retain(|c| c != code);
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.small_button("Tout").clicked() {
                        self.selected_editions = eds_list.clone();
                    }
                    if ui.small_button("Aucune").clicked() {
                        self.selected_editions.clear();
                    }
                });
                // Hors de la rangée horizontale : le texte long se replie tout seul
                // au lieu de sortir de l'écran.
                ui.label(RichText::new("NB : l'API prend une seule édition par téléchargement ; « Tout » enchaîne sur la première. Les éditions virtuelles se choisissent à l'étape 3.").weak().small());
            } else {
                ui.label(RichText::new("Choisissez d'abord une langue.").weak());
            }
        });
        ui.add_space(8.0);

        // Estimation
        card(ui, "Estimation", |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Estimer la taille du téléchargement").clicked() && !self.estimate_busy
                {
                    self.estimate_size();
                }
                if self.estimate_busy {
                    ui.spinner();
                }
                if let Some((name, bytes, n)) = &self.estimate {
                    ui.label(
                        RichText::new(format!("{name} — {n} fichiers — {}", human_size(*bytes)))
                            .strong(),
                    );
                }
            });
        });

        ui.add_space(12.0);
        let ok =
            self.selected.is_some() && !self.lang.is_empty() && !self.selected_editions.is_empty();
        match footer(
            ui,
            "Retour aux builds",
            Some(("Continuer vers Options", ok)),
            accent,
        ) {
            Some(false) => self.page = Page::Search,
            Some(true) => self.page = Page::Options,
            None => {}
        }
    }

    // --------------------------------------------------------------- page 3

    fn page_options(&mut self, ui: &mut egui::Ui, accent: Color32) {
        // Destination
        card(ui, "Destination", |ui| {
            ui.horizontal_wrapped(|ui| {
                let w = field_w(ui, 130.0, 160.0, 430.0);
                ui.add_sized(
                    [w, 22.0],
                    egui::TextEdit::singleline(&mut self.cfg.output_dir),
                );
                if ui.button("Parcourir…").clicked() {
                    if let Some(d) = pick_folder() {
                        self.cfg.output_dir = d;
                    }
                }
            });
        });
        ui.add_space(8.0);

        // ISO & options de conversion — hiérarchie rendue par l'indentation :
        // ISO → AddUpdates → Cleanup → ResetBase ; les autres options au 1er niveau.
        card(ui, "Création de l'ISO", |ui| {
            let o = &mut self.cfg.options;
            ui.checkbox(
                &mut o.convert_iso,
                "Convertir en ISO à la fin du téléchargement (sinon : fichiers UUP seuls)",
            );
            ui.add_enabled_ui(o.convert_iso, |ui| {
                ui.indent("iso_opts", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Format :");
                        ui.selectable_value(
                            &mut o.compression,
                            Compression::Wim,
                            "install.wim (standard)",
                        );
                        ui.selectable_value(
                            &mut o.compression,
                            Compression::Esd,
                            "install.esd (plus compact)",
                        );
                    });
                    ui.checkbox(
                        &mut o.add_updates,
                        "Intégrer les mises à jour cumulatives (AddUpdates)",
                    );
                    ui.add_enabled_ui(o.add_updates, |ui| {
                        ui.indent("upd_opts", |ui| {
                            ui.checkbox(
                                &mut o.cleanup,
                                "Nettoyage des composants obsolètes (Cleanup — l'« auto-clean » du média)",
                            );
                            ui.add_enabled_ui(o.cleanup, |ui| {
                                ui.indent("cb_opts", |ui| {
                                    ui.checkbox(
                                        &mut o.reset_base,
                                        "ResetBase (encore plus compact, mais mises à jour non désinstallables)",
                                    );
                                });
                            });
                        });
                    });
                    ui.checkbox(&mut o.netfx3, "Intégrer .NET Framework 3.5 (NetFx3)");
                    ui.checkbox(
                        &mut o.virtual_editions,
                        "Créer les éditions virtuelles (Enterprise, Education…)",
                    );
                    ui.checkbox(&mut o.skip_edge, "Supprimer Microsoft Edge (SkipEdge)");
                    ui.checkbox(&mut o.skip_winre, "Ne pas recréer winre.wim (SkipWinRE)");
                    ui.checkbox(
                        &mut o.auto_clean,
                        "Auto-clean : supprimer les fichiers UUP temporaires après création de l'ISO",
                    );
                });
            });
        });
        ui.add_space(8.0);

        // Drivers
        card(ui, "Drivers (injection dans l'ISO)", |ui| {
            ui.label(RichText::new(
                "Ajoutez des dossiers contenant des fichiers .inf. Ils sont copiés dans le dossier Drivers/ du convertisseur officiel.",
            ).weak().small());
            ui.add_space(2.0);
            let mut to_remove: Option<usize> = None;
            let mut to_add: Option<(String, DriverTarget)> = None;
            for (i, d) in self.cfg.drivers.iter_mut().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    let w = field_w(ui, 330.0, 140.0, 360.0);
                    ui.add_sized([w, 20.0], egui::TextEdit::singleline(&mut d.path));
                    if ui
                        .selectable_value(&mut d.target, DriverTarget::All, "Toutes images")
                        .clicked()
                    {}
                    if ui
                        .selectable_value(&mut d.target, DriverTarget::Os, "OS")
                        .clicked()
                    {}
                    if ui
                        .selectable_value(&mut d.target, DriverTarget::WinPe, "WinPE")
                        .clicked()
                    {}
                    if ui.button("Dossier…").clicked() {
                        if let Some(p) = pick_folder() {
                            d.path = p;
                        }
                    }
                    if ui.button("Retirer").clicked() {
                        to_remove = Some(i);
                    }
                });
            }
            if let Some(i) = to_remove {
                self.cfg.drivers.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.button("+ Ajouter un dossier de drivers").clicked() {
                    if let Some(p) = pick_folder() {
                        to_add = Some((p, DriverTarget::All));
                    }
                }
            });
            if let Some((p, t)) = to_add {
                self.cfg.drivers.push(DriverEntry { path: p, target: t });
            }
            if !self.cfg.drivers.is_empty() {
                ui.label(RichText::new("Les drivers « Toutes images » vont dans Drivers/ALL (install.wim de toutes les éditions), « OS » dans Drivers/OS, « WinPE » dans Drivers/WinPE (setup).").weak().small());
            }
        });

        ui.add_space(12.0);
        let ok = self.selected.is_some();
        match footer(
            ui,
            "Retour au package",
            Some(("Lancer le téléchargement", ok)),
            accent,
        ) {
            Some(false) => self.page = Page::Package,
            Some(true) => self.start_job(),
            None => {}
        }
    }

    // --------------------------------------------------------------- page 4

    fn page_download(&mut self, ui: &mut egui::Ui, accent: Color32) {
        let (phase, done_b, tot_b, files_done, files_total, speed) = {
            let s = self.job.lock().unwrap();
            (
                s.phase.clone(),
                s.bytes_done,
                s.bytes_total,
                s.files_done,
                s.files_total,
                s.speed_bps,
            )
        };

        card(ui, "Progression", |ui| match &phase {
            Phase::Idle => {
                ui.label("Aucun job en cours.");
                if primary(ui, "Relancer", accent, true) {
                    self.start_job();
                }
            }
            Phase::Done => {
                ui.label(RichText::new("Terminé").color(accent).size(16.0).strong());
                let (iso, work) = {
                    let s = self.job.lock().unwrap();
                    (s.iso_path.clone(), s.work_dir.clone())
                };
                if let Some(iso) = &iso {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("ISO :");
                        ui.label(RichText::new(iso).strong());
                    });
                }
                if let Some(w) = &work {
                    ui.label(RichText::new(format!("Dossier : {w}")).weak().small());
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("Ouvrir le dossier").clicked() {
                        if let Some(w) = &work {
                            let _ = std::process::Command::new(if cfg!(target_os = "windows") {
                                "explorer"
                            } else {
                                "xdg-open"
                            })
                            .arg(w)
                            .spawn();
                        }
                    }
                    if ui.button("Nouveau téléchargement").clicked() {
                        self.job_running = false;
                        self.page = Page::Search;
                    }
                });
            }
            Phase::Failed(e) => {
                ui.label(
                    RichText::new(format!("Échec : {e}"))
                        .color(Color32::from_rgb(224, 96, 96))
                        .strong(),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("Retour aux options").clicked() {
                        self.job_running = false;
                        self.page = Page::Options;
                    }
                    if primary(ui, "Réessayer", accent, true) {
                        self.start_job();
                    }
                });
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    match phase {
                        Phase::FetchingList => {
                            ui.label("Récupération de la liste de fichiers…");
                        }
                        Phase::Preparing => {
                            ui.label("Préparation du convertisseur officiel…");
                        }
                        Phase::Downloading => {
                            ui.label(format!(
                                "Téléchargement : {}/{} fichiers — {} / {}",
                                files_done,
                                files_total,
                                human_size(done_b),
                                human_size(tot_b)
                            ));
                        }
                        Phase::Converting => {
                            ui.label("Conversion ISO en cours…");
                        }
                        Phase::CleaningUp => {
                            ui.label("Nettoyage automatique…");
                        }
                        _ => {}
                    }
                });
                let frac = if tot_b > 0 {
                    (done_b as f32 / tot_b as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .show_percentage()
                        .desired_height(18.0),
                );
                ui.horizontal_wrapped(|ui| {
                    if speed > 0 {
                        ui.label(RichText::new(format!("{}/s", human_size(speed))).weak());
                    }
                    if let Some(w) = self.job.lock().unwrap().work_dir.clone() {
                        ui.separator();
                        ui.label(RichText::new(w).weak().small());
                    }
                });
                ui.add_space(2.0);
                if ui.button("Annuler").clicked() {
                    self.cancel.store(true, Ordering::SeqCst);
                }
            }
        });

        ui.add_space(8.0);
        // Journal — fond « terminal », colle au bas ; hauteur bornée pour que
        // la page reste lisible même en fenêtre basse (la page défile).
        ui.label(RichText::new("Journal").strong().small());
        let lines: Vec<String> = self.job.lock().unwrap().log.clone();
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(6.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.set_min_height(220.0);
                        for l in lines
                            .iter()
                            .rev()
                            .take(300)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                        {
                            ui.monospace(RichText::new(l).small());
                        }
                    });
            });
    }
}

fn chrono_days_ago(ts: i64) -> String {
    // Affichage simple et sans dépendance : âge relatif.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = (now - ts) / 86400;
    if days <= 0 {
        "aujourd'hui".into()
    } else if days == 1 {
        "hier".into()
    } else if days < 60 {
        format!("il y a {days} j")
    } else {
        format!("il y a {} mois", days / 30)
    }
}

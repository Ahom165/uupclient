//! Rendu de l'interface (egui) — volontairement sobre : fonds neutres,
//! une seule couleur d'accent, typographie hiérarchisée, zéro décor superflu.

use eframe::egui;
use eframe::egui::{
    Color32, ComboBox, Context, CornerRadius, Label, Layout, RichText, ScrollArea, Sense, Ui,
};

use crate::app::{App, Screen};
use crate::api::{ARCHS, CHANNELS};
use crate::downloader::FileState;
use crate::models::DlMode;
use crate::util;

const ACCENT: Color32 = Color32::from_rgb(0x4e, 0x8f, 0x78);
const ERR: Color32 = Color32::from_rgb(0xc9, 0x6a, 0x5a);
const DIM: Color32 = Color32::from_rgb(0x8a, 0x8f, 0x94);

pub fn render(ctx: &Context, app: &mut App) {
    header(ctx, app);
    // Barre de recherche épinglée en haut (panneau fixe) : elle ne défile jamais
    // avec les résultats, quelle que soit la hauteur de la liste.
    if app.screen == Screen::Search {
        search_bar_panel(ctx, app);
    }
    if app.show_settings {
        settings_panel(ctx, app);
    }
    // Barre d'action épinglée en bas de l'écran Détail : le bouton Télécharger
    // reste visible même quand la liste des éditions déborde.
    if app.screen == Screen::Detail && app.current_build().is_some() {
        detail_action_panel(ctx, app);
    }
    egui::CentralPanel::default().show(ctx, |ui| match app.screen {
        Screen::Search => results_screen(ui, app),
        Screen::Detail => detail_screen(ui, app),
        Screen::Downloads => downloads_screen(ui, app),
        Screen::Finished => finished_screen(ui, app),
    });
    bottom_log(ctx, app);
}

// ---------------------------------------------------------------- header

fn header(ctx: &Context, app: &mut App) {
    // exact_height : sans lui, egui 0.33 peut mémoriser une hauteur erronée pour
    // le panneau dès la première interaction pointeur et l'écran "descend" alors
    // à chaque repaint (panneau qui gonfle indéfiniment).
    egui::TopBottomPanel::top("header")
        .exact_height(42.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.strong(RichText::new("UUP dump Client").size(18.0));
                ui.label(RichText::new("· client natif léger").color(DIM).small());
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .selectable_label(app.show_settings, RichText::new("⚙ Paramètres"))
                        .clicked()
                    {
                        app.show_settings = !app.show_settings;
                    }
                    if app.screen != Screen::Search && ui.button("← Rechercher").clicked() {
                        app.back_to_search();
                    }
                });
            });
            ui.add_space(5.0);
            ui.separator();
        });
}

// ---------------------------------------------------------------- paramètres

fn settings_panel(ctx: &Context, app: &mut App) {
    egui::SidePanel::right("settings")
        .resizable(false)
        .default_width(320.0)
        .show(ctx, |ui| {
            ui.heading(RichText::new("Paramètres").size(16.0));
            ui.separator();
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let mut changed = false;

                // Destination
                ui.strong("Destination");
                let dest_txt = app
                    .settings
                    .dest_dir
                    .clone()
                    .unwrap_or_else(|| "Aucun dossier choisi".into());
                ui.add(
                    Label::new(RichText::new(&dest_txt).small().color(DIM)).wrap_mode(egui::TextWrapMode::Truncate),
                );
                ui.horizontal(|ui| {
                    if ui.button("Parcourir…").clicked() {
                        if let Some(d) = rfd::FileDialog::new()
                            .set_title("Dossier de destination")
                            .pick_folder()
                        {
                            app.settings.dest_dir = Some(d.to_string_lossy().to_string());
                            changed = true;
                        }
                    }
                    if let Some(root) = app.project_root() {
                        if root.exists() && ui.button("Ouvrir").clicked() {
                            util::open_folder(&root);
                        }
                    }
                });
                ui.add_space(8.0);

                // Téléchargement
                ui.strong("Téléchargement");
                if ui
                    .add(
                        egui::Slider::new(&mut app.settings.options.threads, 1..=8)
                            .text("Connexions parallèles"),
                    )
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.options.verify_hashes,
                        "Vérifier les empreintes SHA-1",
                    )
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.options.store_apps,
                        "Inclure les apps du Microsoft Store",
                    )
                    .changed()
                {
                    changed = true;
                }
                ui.add_space(8.0);

                // Création
                ui.strong("Création de l'ISO");
                if ui
                    .checkbox(&mut app.settings.options.make_iso, "Convertir en ISO")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(&mut app.settings.options.add_updates, "Intégrer les mises à jour (AddUpdates)")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.options.cleanup,
                        "Auto-clean des fichiers temporaires (Cleanup)",
                    )
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(&mut app.settings.options.reset_base, "ResetBase (réduit la taille, plus long)")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(&mut app.settings.options.skip_edge, "Ne pas réintégrer Edge (SkipEdge)")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(&mut app.settings.options.esd, "Compression ESD (plus léger, plus long)")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.options.virtual_editions,
                        "Créer les éditions virtuelles (Enterprise…)",
                    )
                    .changed()
                {
                    changed = true;
                }
                ui.add_space(8.0);

                // Drivers
                ui.strong("Drivers à intégrer (Windows + WinPE)");
                if app.settings.drivers.is_empty() {
                    ui.label(RichText::new("Aucun dossier de drivers ajouté.").color(DIM).small());
                }
                let mut to_remove: Option<usize> = None;
                for (i, d) in app.settings.drivers.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(
                            Label::new(RichText::new(d).small()).wrap_mode(egui::TextWrapMode::Truncate),
                        );
                        if ui.small_button("✕").clicked() {
                            to_remove = Some(i);
                        }
                    });
                }
                if let Some(i) = to_remove {
                    app.settings.drivers.remove(i);
                    changed = true;
                }
                if ui.button("+ Ajouter un dossier de drivers").clicked() {
                    if let Some(d) = rfd::FileDialog::new()
                        .set_title("Dossier contenant les fichiers .inf")
                        .pick_folder()
                    {
                        let path = d.to_string_lossy().to_string();
                        if !app.settings.drivers.contains(&path) {
                            app.settings.drivers.push(path);
                            changed = true;
                        }
                    }
                }
                ui.label(
                    RichText::new(
                        "Copiés vers Drivers/ALL/ (recherche récursive des .inf) : intégrés aux images Windows ET WinPE/WinRE via AddDrivers dans ConvertConfig.ini.",
                    )
                    .small()
                    .color(DIM),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.label(
                    RichText::new(
                        "La création d'ISO sous Windows demande les droits administrateur (DISM).",
                    )
                    .small()
                    .color(DIM),
                );

                if changed {
                    app.save_settings();
                }
            });
        });
}

// ---------------------------------------------------------------- recherche

fn search_bar_panel(ctx: &Context, app: &mut App) {
    egui::TopBottomPanel::top("search_bar")
        .exact_height(72.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            egui::Frame::new()
            .corner_radius(CornerRadius::same(6))
            .fill(Color32::from_additive_luminance(6))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let w = ui.available_width();
                    ui.add_sized(
                        [w - 420.0, 26.0],
                        egui::TextEdit::singleline(&mut app.query)
                            .hint_text("N° de build ou mots-clés (ex. 26200, insider, server…)"),
                    );
                    ComboBox::from_id_salt("chan")
                        .selected_text(channel_label(&app.settings.channel))
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            for (code, label) in CHANNELS {
                                ui.selectable_value(&mut app.settings.channel, code.to_string(), *label);
                            }
                        });
                    ComboBox::from_id_salt("arch")
                        .selected_text(app.settings.arch.clone())
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for a in ARCHS {
                                ui.selectable_value(&mut app.settings.arch, a.to_string(), *a);
                            }
                        });
                    if ui.button("Dernières du canal").clicked() {
                        app.query.clear();
                        app.save_settings();
                        app.search();
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Rechercher").strong()).fill(ACCENT))
                        .clicked()
                    {
                        app.save_settings();
                        app.search();
                    }
                });
                ui.label(
                    RichText::new(
                        "Champ vide + « Rechercher » = dernières builds toutes canaux confondus.",
                    )
                    .small()
                    .color(DIM),
                );
            });
    });
}

fn results_screen(ui: &mut Ui, app: &mut App) {
    ui.add_space(8.0);

    if let Some(b) = &app.busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(b);
        });
    }
    if let Some(e) = &app.error {
        ui.colored_label(ERR, format!("⚠ {e}"));
    }

    ui.strong(format!("{} build(s)", app.results.len()));
    ScrollArea::vertical()
        .id_salt("results")
        .auto_shrink([false, false])
        .show(ui, |ui| {
        let mut open_idx: Option<usize> = None;
        for (i, b) in app.results.iter().enumerate() {
            let sel = app.selected == Some(i);
            let resp = egui::Frame::new()
                .corner_radius(CornerRadius::same(5))
                .fill(if sel {
                    Color32::from_additive_luminance(14)
                } else {
                    Color32::from_additive_luminance(4)
                })
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.strong(b.title.clone());
                            ui.label(
                                RichText::new(format!(
                                    "{} · {} · {}",
                                    b.build,
                                    b.arch,
                                    util::fmt_date(b.created)
                                ))
                                .small()
                                .color(DIM),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(short_uuid(&b.uuid)).small().color(DIM));
                        });
                    });
                })
                .response
                .interact(Sense::click());
            if resp.clicked() {
                open_idx = Some(i);
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            ui.add_space(2.0);
        }
        if let Some(i) = open_idx {
            app.open_build(i);
        }
    });
}

fn channel_label(code: &str) -> String {
    CHANNELS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| code.to_string())
}

fn short_uuid(u: &str) -> String {
    u.chars().take(8).collect()
}

// ---------------------------------------------------------------- détail

fn detail_screen(ui: &mut Ui, app: &mut App) {
    ui.add_space(10.0);
    let Some(build) = app.current_build() else {
        app.back_to_search();
        return;
    };

    ui.strong(RichText::new(&build.title).size(17.0));
    ui.label(
        RichText::new(format!(
            "Build {} · {} · publiée le {} · id {}",
            build.build,
            build.arch,
            util::fmt_date(build.created),
            build.uuid
        ))
        .small()
        .color(DIM),
    );
    ui.separator();

    // Zone défilante : seul ce bloc bouge — le titre reste en haut et la
    // barre d'action (bouton Télécharger) reste épinglée en bas de l'écran.
    ScrollArea::vertical()
        .id_salt("detail")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(2.0);

            if let Some(b) = &app.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(b);
                });
                ui.add_space(4.0);
            }
            if let Some(e) = &app.error {
                ui.colored_label(ERR, format!("⚠ {e}"));
                ui.add_space(4.0);
            }

            // Mode
            ui.horizontal(|ui| {
                ui.radio_value(&mut app.mode, DlMode::Full, "Set complet (ISO possible)");
                ui.radio_value(&mut app.mode, DlMode::UpdatesOnly, "Mises à jour uniquement");
            });

            match app.mode {
                DlMode::UpdatesOnly => {
                    ui.add_space(6.0);
                    ui.label(
                        "Télécharge uniquement les fichiers de mise à jour (cabinets/MSU) de cette build, sans image d'installation.",
                    );
                }
                DlMode::Full => {
                    ui.add_space(6.0);

                    // Langue
                    ui.strong("Langue");
                    if let Some(langs) = &app.langs {
                        let sel = langs
                            .get(app.lang_idx)
                            .map(|l| format!("{} ({})", l.fancy, l.code))
                            .unwrap_or_else(|| "—".into());
                        let mut new_idx: Option<usize> = None;
                        ComboBox::from_id_salt("lang")
                            .selected_text(sel)
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for (i, l) in langs.iter().enumerate() {
                                    if ui
                                        .selectable_label(i == app.lang_idx, format!("{} ({})", l.fancy, l.code))
                                        .clicked()
                                    {
                                        new_idx = Some(i);
                                    }
                                }
                            });
                        if let Some(i) = new_idx {
                            app.lang_chosen(i);
                        }
                    } else {
                        ui.label(RichText::new("Chargement des langues…").color(DIM));
                    }

                    ui.add_space(6.0);

                    // Éditions (dans le défilement global, pas de scroll imbriqué)
                    ui.strong("Éditions");
                    ui.horizontal(|ui| {
                        if ui.small_button("Tout cocher").clicked() {
                            for (_, on) in app.editions.iter_mut() {
                                *on = true;
                            }
                        }
                        if ui.small_button("Tout décocher").clicked() {
                            for (_, on) in app.editions.iter_mut() {
                                *on = false;
                            }
                        }
                        if ui.small_button("Windows Pro").clicked() {
                            for (e, on) in app.editions.iter_mut() {
                                *on = e.key.eq_ignore_ascii_case("professional");
                            }
                        }
                    });
                    let count = app.editions.len();
                    let cols = 3usize;
                    egui::Grid::new("edgrid")
                        .num_columns(cols)
                        .min_col_width(140.0)
                        .show(ui, |ui| {
                            for chunk_start in (0..count).step_by(cols) {
                                for j in 0..cols {
                                    let i = chunk_start + j;
                                    if i < count {
                                        let name = app.editions[i].0.fancy.clone();
                                        let mut val = app.editions[i].1;
                                        if ui.checkbox(&mut val, name).changed() {
                                            app.editions[i].1 = val;
                                        }
                                    } else {
                                        ui.label("");
                                    }
                                }
                                ui.end_row();
                            }
                        });
                }
            }
        });
}

/// Barre d'action de l'écran Détail : épinglée en bas (panneau fixe de hauteur
/// exacte), le bouton Télécharger et le résumé restent toujours visibles.
fn detail_action_panel(ctx: &Context, app: &mut App) {
    egui::TopBottomPanel::bottom("detail_actions")
        .exact_height(66.0)
        .show(ctx, |ui| {
            ui.separator();
            ui.add_space(5.0);

            let dest = app
            .settings
            .dest_dir
            .clone()
            .unwrap_or_else(|| "destination non définie".into());
        ui.label(
            RichText::new(format!(
                "Destination : {} · {} driver(s) · ISO : {} · auto-clean : {}",
                dest,
                app.settings.drivers.len(),
                if app.settings.options.make_iso { "oui" } else { "non" },
                if app.settings.options.cleanup { "oui" } else { "non" },
            ))
            .small()
            .color(DIM),
        );

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let can = app.busy.is_none();
            let label = match app.mode {
                DlMode::Full => "Télécharger les fichiers UUP",
                DlMode::UpdatesOnly => "Télécharger les mises à jour",
            };
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(RichText::new(label).strong().size(15.0)).fill(ACCENT),
                )
                .clicked()
            {
                app.start_download();
            }
        });
        ui.add_space(4.0);
    });
}

// ---------------------------------------------------------------- téléchargement

fn downloads_screen(ui: &mut Ui, app: &mut App) {
    ui.add_space(10.0);
    ui.strong(RichText::new(format!("Téléchargement — {}", app.dl.title)).size(16.0));
    ui.add_space(4.0);

    let pct = app.dl.progress();
    ui.add(
        egui::ProgressBar::new(pct)
            .show_percentage()
            .desired_height(18.0),
    );
    ui.label(
        RichText::new(format!(
            "{} / {} fichiers · {} / {} · {}",
            app.dl.files_done(),
            app.dl.items.len(),
            util::fmt_size(app.dl.total_done),
            util::fmt_size(app.dl.total_bytes),
            util::fmt_speed(app.dl.speed),
        ))
        .color(DIM),
    );

    ui.add_space(6.0);
    let dl_active = app.dl.active;
    let dl_finished = app.dl.finished;
    let dl_cancelled = app.dl.cancelled;
    let dl_failed_count = app.dl.failed.len();
    ui.horizontal(|ui| {
        if dl_active && ui.button("Annuler").clicked() {
            app.cancel_download();
        }
        if dl_finished && (dl_cancelled || dl_failed_count > 0) && ui.button("Reprendre").clicked() {
            app.resume_download();
        }
        if dl_finished && dl_failed_count == 0 && !dl_cancelled {
            if app.settings.options.make_iso && app.mode == DlMode::Full {
                if ui
                    .add(egui::Button::new(RichText::new("Créer l'ISO").strong()).fill(ACCENT))
                    .clicked()
                {
                    app.start_build();
                }
            } else if let Some((_, project)) = app.last_plan_clone() {
                if ui.button("Ouvrir le dossier").clicked() {
                    util::open_folder(&project);
                }
            }
        }
    });

    ui.separator();
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let items_snapshot: Vec<(String, u64, FileState, u64)> = app
            .dl
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                (
                    item.name.clone(),
                    item.size,
                    app.dl.states[i].clone(),
                    app.dl.done[i],
                )
            })
            .collect();
        for (name, size, state, done) in &items_snapshot {
            let (icon, color) = match state {
                FileState::Pending => ("…", DIM),
                FileState::Downloading => ("↓", ACCENT),
                FileState::Done => ("✓", ACCENT),
                FileState::Failed(_) => ("✗", ERR),
            };
            ui.horizontal(|ui| {
                ui.colored_label(color, icon);
                ui.add(
                    Label::new(RichText::new(name).small()).wrap_mode(egui::TextWrapMode::Truncate),
                );
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let done_txt = match state {
                        FileState::Downloading => {
                            format!("{} / {}", util::fmt_size(*done), util::fmt_size(*size))
                        }
                        _ => util::fmt_size(*size),
                    };
                    ui.label(RichText::new(done_txt).small().color(DIM));
                });
            });
        }
    });
}

// ---------------------------------------------------------------- fin (build ISO)

fn finished_screen(ui: &mut Ui, app: &mut App) {
    ui.add_space(10.0);

    if app.build.active {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.strong(RichText::new(format!("Création de l'ISO — {}", app.build.stage)).size(15.0));
        });
    } else if let Some(iso) = app.build.iso.clone() {
        ui.strong(RichText::new("✓ ISO prête").size(18.0).color(ACCENT));
        ui.label(RichText::new(iso.to_string_lossy().to_string()).monospace());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(RichText::new("Ouvrir le dossier").strong()).fill(ACCENT))
                .clicked()
            {
                if let Some(p) = iso.parent() {
                    util::open_folder(p);
                }
            }
            if ui.button("Nouvelle recherche").clicked() {
                app.reset_for_new_search();
            }
        });
        if app.settings.options.cleanup {
            ui.label(
                RichText::new("L'auto-clean a supprimé UUPs/ et files/ dans le dossier du projet.")
                    .small()
                    .color(DIM),
            );
        }
        ui.separator();
        show_build_logs(ui, app);
        return;
    } else if let Some(err) = &app.build.error {
        ui.strong(RichText::new("Échec de la création").size(16.0).color(ERR));
        ui.colored_label(ERR, err);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Réessayer la conversion").clicked() {
                app.build.error = None;
                app.start_build();
            }
            if ui.button("Nouvelle recherche").clicked() {
                app.reset_for_new_search();
            }
        });
        ui.separator();
        show_build_logs(ui, app);
        return;
    } else {
        ui.strong("Prêt.");
    }

    ui.separator();
    show_build_logs(ui, app);
}

fn show_build_logs(ui: &mut Ui, app: &mut App) {
    ScrollArea::vertical()
        .id_salt("buildlogs")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for l in &app.build.logs {
                ui.add(Label::new(RichText::new(l).small().monospace()).wrap_mode(egui::TextWrapMode::Wrap));
            }
        });
}

// ---------------------------------------------------------------- journal

fn bottom_log(ctx: &Context, app: &mut App) {
    // Panneau à hauteur exacte (repli ou déplié) : immunisé contre le bug de
    // mémoire de panneau d'egui 0.33 qui gonfle les panneaux interagis.
    egui::TopBottomPanel::bottom("logbar")
        .exact_height(if app.show_log { 196.0 } else { 28.0 })
        .show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                let arrow = if app.show_log { "▾" } else { "▸" };
                if ui
                    .small_button(RichText::new(format!("{arrow} Journal")).small())
                    .clicked()
                {
                    app.show_log = !app.show_log;
                }
                if !app.show_log {
                    if let Some(l) = app.logs.back() {
                        ui.add(
                            Label::new(RichText::new(l).small().color(DIM))
                                .wrap_mode(egui::TextWrapMode::Truncate),
                        );
                    }
                }
            });
            if app.show_log {
                ScrollArea::vertical()
                    .id_salt("applogs")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &app.logs {
                            ui.add(
                                Label::new(RichText::new(l).small().monospace())
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                        }
                    });
            }
        });
}

fn last_log(logs: &std::collections::VecDeque<String>) -> String {
    logs.back().cloned().unwrap_or_default()
}

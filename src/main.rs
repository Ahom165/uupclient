//! UUP dump Client — client natif léger en Rust.
//! Recherche de builds Windows via l'API UUP dump, configuration du package,
//! intégration de drivers et création d'ISO via le convertisseur officiel.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod job;
mod store;
mod ui;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Taille par défaut ; surcharge possible pour tests : UUPDUMP_SIZE=700x520
    let mut size = [1000.0, 680.0];
    if let Ok(s) = std::env::var("UUPDUMP_SIZE") {
        let mut it = s.split(['x', 'X', '*']);
        if let (Some(w), Some(h)) = (it.next(), it.next()) {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<f32>(), h.trim().parse::<f32>()) {
                size = [w.max(320.0), h.max(240.0)];
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([560.0, 400.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "UUP dump Client",
        options,
        Box::new(|cc| {
            // Police plus confortable : ajuste la taille de texte par défaut.
            let mut style = (*cc.egui_ctx.style()).clone();
            style.text_styles = [
                (egui::TextStyle::Body, egui::FontId::proportional(14.5)),
                (egui::TextStyle::Button, egui::FontId::proportional(14.5)),
                (egui::TextStyle::Heading, egui::FontId::proportional(21.0)),
                (egui::TextStyle::Monospace, egui::FontId::monospace(12.5)),
                (egui::TextStyle::Small, egui::FontId::proportional(11.5)),
            ]
            .into();
            cc.egui_ctx.set_style(style);
            Ok(Box::new(ui::App::new()))
        }),
    )
}

fn load_icon() -> egui::IconData {
    // Icône disque simple encodée en pixels (16×16) : sobre, aucune dépendance.
    const W: usize = 16;
    const H: usize = 16;
    let mut rgba = vec![0u8; W * H * 4];
    let put = |rgba: &mut Vec<u8>, x: usize, y: usize, c: [u8; 4]| {
        let i = (y * W + x) * 4;
        rgba[i..i + 4].copy_from_slice(&c);
    };
    for y in 2..14 {
        for x in 2..14 {
            let edge = x == 2 || x == 13 || y == 2 || y == 13;
            let band = y == 9 || y == 10;
            let c = if edge {
                [96u8, 148, 214, 255]
            } else if band {
                [96u8, 148, 214, 255]
            } else {
                [236u8, 240, 246, 255]
            };
            put(&mut rgba, x, y, c);
        }
    }
    egui::IconData {
        width: W as u32,
        height: H as u32,
        rgba,
    }
}

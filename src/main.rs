//! Point d'entrée — client natif léger pour UUP dump (egui/eframe).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod builder;
mod config;
mod downloader;
mod models;
mod ui;
mod util;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([880.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "UUP dump Client",
        options,
        Box::new(|cc| {
            // Thème sobre : sombre neutre, un seul accent.
            use eframe::egui::Visuals;
            let mut v = Visuals::dark();
            v.panel_fill = eframe::egui::Color32::from_rgb(0x1b, 0x1d, 0x1f);
            v.window_fill = eframe::egui::Color32::from_rgb(0x20, 0x22, 0x25);
            v.extreme_bg_color = eframe::egui::Color32::from_rgb(0x14, 0x15, 0x17);
            v.selection.bg_fill = eframe::egui::Color32::from_rgb(0x2e, 0x50, 0x43);
            v.selection.stroke.color = eframe::egui::Color32::WHITE;
            v.widgets.hovered.bg_fill = eframe::egui::Color32::from_rgb(0x2a, 0x2d, 0x30);
            v.widgets.active.bg_fill = eframe::egui::Color32::from_rgb(0x33, 0x37, 0x3a);
            v.widgets.inactive.bg_fill = eframe::egui::Color32::from_rgb(0x24, 0x26, 0x29);
            cc.egui_ctx.set_visuals(v);

            Ok(Box::new(app::App::new()))
        }),
    )
}

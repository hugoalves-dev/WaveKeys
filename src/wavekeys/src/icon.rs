// Copyright (c) Modding Forge

use std::sync::Arc;

const ICON_SIZE: u32 = 64;
const ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/waveinput-64.rgba"));

pub fn tray_icon() -> Result<tray_icon::Icon, String> {
    tray_icon::Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE)
        .map_err(|error| error.to_string())
}

pub fn egui_icon() -> Arc<eframe::egui::IconData> {
    Arc::new(eframe::egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: ICON_SIZE,
        height: ICON_SIZE,
    })
}

pub fn color_image() -> eframe::egui::ColorImage {
    eframe::egui::ColorImage::from_rgba_unmultiplied(
        [ICON_SIZE as usize, ICON_SIZE as usize],
        ICON_RGBA,
    )
}

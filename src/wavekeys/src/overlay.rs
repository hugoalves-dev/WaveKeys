// Copyright (c) Modding Forge

use base64::{engine::general_purpose::STANDARD, Engine as _};
use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::{Receiver, Sender},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use wavekeys_core::{Config, MixTarget, VolumeTargetIcon, VolumeUpdate};
use windows::core::w;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{
            DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW,
            DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_SYSTEMBACKDROP_TYPE,
            DWM_WINDOW_CORNER_PREFERENCE,
        },
        Gdi::{
            GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
            MONITOR_DEFAULTTONEAREST,
        },
    },
    UI::HiDpi::GetDpiForWindow,
    UI::WindowsAndMessaging::{
        CallWindowProcW, FindWindowW, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW,
        GetWindowRect, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
        ShowWindow, GWLP_WNDPROC, GWL_EXSTYLE, HTCAPTION, HTTRANSPARENT, HWND_TOPMOST,
        MA_NOACTIVATE, SC_MOVE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
        WM_MOUSEACTIVATE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_SYSCOMMAND, WNDPROC, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    },
};

const OVERLAY_WIDTH: f32 = 430.0;
const OVERLAY_HEIGHT_WITH_NAME: f32 = 112.0;
const OVERLAY_HEIGHT_WITHOUT_NAME: f32 = 84.0;
const TASKBAR_GAP: f32 = 20.0;
const OVERLAY_BACKGROUND_ALPHA: u8 = 0;
const DEFAULT_LAYOUT_SCALE: f32 = 0.7;
const DEFAULT_TEXT_SCALE: f32 = 1.5;
const PREVIEW_BACKGROUND_ALPHA: u8 = 255;
static ORIGINAL_WINDOW_PROC: OnceLock<isize> = OnceLock::new();
static SETTINGS_INTERACTIVE: AtomicBool = AtomicBool::new(false);
static OVERLAY_FONTS: OnceLock<Mutex<BTreeMap<String, Vec<u8>>>> = OnceLock::new();

/// Commands sent by the tray window to the shared UI event loop.
pub enum OverlayCommand {
    OpenSettings,
    Quit,
}

fn show_hidden_ui_window(window: &AtomicIsize) {
    let address = window.load(Ordering::Acquire);
    if address != 0 {
        unsafe {
            let _ = ShowWindow(HWND(address as *mut _), SW_SHOWNOACTIVATE);
        }
    }
}

enum WindowMode {
    ArmClickthrough,
    DisableClickthrough,
}

/// Sends a level and explicitly wakes the UI event loop.
pub struct VolumeOverlaySender {
    sender: Sender<VolumeUpdate>,
    context: Arc<Mutex<Option<egui::Context>>>,
    window: Arc<AtomicIsize>,
}

impl VolumeOverlaySender {
    pub fn send(&self, update: VolumeUpdate) {
        if self.sender.send(update).is_ok() {
            show_hidden_ui_window(&self.window);
            if let Some(context) = self.context.lock().ok().and_then(|guard| guard.clone()) {
                context.request_repaint();
            }
        }
    }
}

pub struct VolumeOverlayReceiver {
    receiver: Receiver<VolumeUpdate>,
    context: Arc<Mutex<Option<egui::Context>>>,
    window: Arc<AtomicIsize>,
}

pub struct OverlayCommandSender {
    sender: Sender<OverlayCommand>,
    context: Arc<Mutex<Option<egui::Context>>>,
    window: Arc<AtomicIsize>,
}

impl OverlayCommandSender {
    pub fn send(&self, command: OverlayCommand) {
        if self.sender.send(command).is_ok() {
            show_hidden_ui_window(&self.window);
            if let Some(context) = self.context.lock().ok().and_then(|guard| guard.clone()) {
                context.request_repaint();
            }
        }
    }
}

impl VolumeOverlayReceiver {
    pub fn command_channel(&self) -> (OverlayCommandSender, Receiver<OverlayCommand>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        (
            OverlayCommandSender {
                sender,
                context: self.context.clone(),
                window: self.window.clone(),
            },
            receiver,
        )
    }
}

pub fn volume_channel() -> (VolumeOverlaySender, VolumeOverlayReceiver) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let context = Arc::new(Mutex::new(None));
    let window = Arc::new(AtomicIsize::new(0));
    (
        VolumeOverlaySender {
            sender,
            context: context.clone(),
            window: window.clone(),
        },
        VolumeOverlayReceiver {
            receiver,
            context,
            window,
        },
    )
}

/// Runs the persistent, initially hidden overlay on the Windows UI thread.
pub fn run(
    volume_channel: VolumeOverlayReceiver,
    commands: Receiver<OverlayCommand>,
) -> Result<(), String> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("WaveKeys Volume Overlay")
            .with_icon(crate::icon::egui_icon())
            .with_inner_size([OVERLAY_WIDTH, OVERLAY_HEIGHT_WITH_NAME])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_active(false)
            .with_visible(false)
            .with_position([-32_000.0, -32_000.0])
            .with_taskbar(false)
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        "WaveKeys Volume Overlay",
        options,
        Box::new(move |creation_context| {
            let window = overlay_window_handle(creation_context)?;
            configure_overlay_window(window);
            volume_channel
                .window
                .store(window.0 as isize, Ordering::Release);
            if let Ok(mut context) = volume_channel.context.lock() {
                *context = Some(creation_context.egui_ctx.clone());
            }
            Ok(Box::new(OverlayApp::new(
                &creation_context.egui_ctx,
                volume_channel.receiver,
                commands,
                window,
            )))
        }),
    )
    .map_err(|error| error.to_string())
}

fn overlay_window_handle(
    creation_context: &eframe::CreationContext<'_>,
) -> Result<HWND, Box<dyn std::error::Error + Send + Sync>> {
    let handle = creation_context.window_handle()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("volume overlay requires a Win32 window".into());
    };
    Ok(HWND(handle.hwnd.get() as *mut _))
}

struct OverlayApp {
    receiver: Receiver<VolumeUpdate>,
    commands: Receiver<OverlayCommand>,
    update: Option<VolumeUpdate>,
    icon_texture: Option<egui::TextureHandle>,
    app_icon_texture: egui::TextureHandle,
    hide_at: Option<Instant>,
    window_mode: Sender<WindowMode>,
    visible: bool,
    window: HWND,
    accent: egui::Color32,
    settings: Option<crate::tray::SettingsApp>,
    settings_monitor: Option<HMONITOR>,
}

impl OverlayApp {
    fn new(
        ctx: &egui::Context,
        receiver: Receiver<VolumeUpdate>,
        commands: Receiver<OverlayCommand>,
        window: HWND,
    ) -> Self {
        let (window_mode, mode_receiver) = std::sync::mpsc::channel();
        start_window_mode_worker(window, mode_receiver);
        let app_icon_texture = ctx.load_texture(
            "waveinput-app-icon",
            crate::icon::color_image(),
            egui::TextureOptions::LINEAR,
        );
        Self {
            receiver,
            commands,
            update: None,
            icon_texture: None,
            app_icon_texture,
            hide_at: None,
            window_mode,
            visible: false,
            window,
            accent: windows_accent(),
            settings: None,
            settings_monitor: None,
        }
    }

    fn receive_commands(&mut self, ctx: &egui::Context) {
        for command in self.commands.try_iter() {
            match command {
                OverlayCommand::OpenSettings => {
                    let monitor = unsafe {
                        MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST)
                    };
                    self.settings_monitor = Some(monitor);
                    self.settings
                        .get_or_insert_with(crate::tray::SettingsApp::new);
                    self.hide_at = None;
                    let _ = self.window_mode.send(WindowMode::DisableClickthrough);
                    self.visible = false;
                    self.update = None;
                    self.icon_texture = None;
                    show_settings_window(self.window, ctx.pixels_per_point(), monitor);
                }
                OverlayCommand::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn render_settings(&mut self, ctx: &egui::Context) -> bool {
        let Some(settings) = &mut self.settings else {
            return false;
        };
        settings.show(ctx);
        let preview_config = settings.config().clone();
        if settings.take_close_requested() {
            self.settings = None;
            self.settings_monitor = None;
            set_settings_interactive(self.window, false);
            unsafe {
                let _ = ShowWindow(self.window, SW_HIDE);
            }
            return false;
        }

        let preview = preview_update(&preview_config);
        let preview_size = overlay_logical_size(preview.show_target_name, preview.scale);
        let accent = self.accent;
        let preview_monitor = self.settings_monitor;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("volume-overlay-live-preview"),
            egui::ViewportBuilder::default()
                .with_title("WaveKeys Live Preview")
                .with_icon(crate::icon::egui_icon())
                .with_inner_size(preview_size)
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(true)
                .with_active(false)
                .with_taskbar(false)
                .with_always_on_top(),
            move |preview_ctx, _class| {
                preview_ctx.set_visuals(egui::Visuals::dark());
                let font_family = install_overlay_font(preview_ctx, &preview.font_family);
                render_preview_overlay(preview_ctx, &preview, accent, &font_family);
                if let Ok(preview_window) =
                    unsafe { FindWindowW(None, w!("WaveKeys Live Preview")) }
                {
                    ensure_overlay_style(preview_window);
                    set_clickthrough(preview_window, true);
                    disable_backdrop(preview_window);
                    position_and_show_on_monitor(
                        preview_window,
                        preview_ctx.pixels_per_point(),
                        preview.show_target_name,
                        preview.scale,
                        preview_monitor,
                    );
                }
            },
        );
        true
    }

    fn receive_latest(&mut self, ctx: &egui::Context) {
        let Some(update) = self.receiver.try_iter().last() else {
            return;
        };
        if self.settings.is_some() {
            return;
        }
        self.icon_texture = channel_texture(ctx, &update.icon);
        set_clickthrough(self.window, false);
        position_and_show(
            self.window,
            ctx.pixels_per_point(),
            update.show_target_name,
            update.scale,
        );
        configure_overlay_backdrop(self.window);
        let dismiss_after = Duration::from_secs_f32(update.hide_delay_seconds.clamp(0.1, 10.0));
        self.update = Some(update);
        self.hide_at = Some(Instant::now() + dismiss_after);
        let _ = self.window_mode.send(WindowMode::ArmClickthrough);
        self.visible = true;
    }

    fn hide_if_idle(&mut self) {
        if self
            .hide_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            unsafe {
                let _ = ShowWindow(self.window, SW_HIDE);
            }
            set_clickthrough(self.window, false);
            self.hide_at = None;
            let _ = self.window_mode.send(WindowMode::DisableClickthrough);
            self.visible = false;
            self.update = None;
            self.icon_texture = None;
        }
    }

    fn render_icon(&self, painter: &egui::Painter, rect: egui::Rect, icon: &VolumeTargetIcon) {
        if let Some(texture) = &self.icon_texture {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            return;
        }
        let scale = rect.width() / 48.0;
        let stroke = egui::Stroke::new(2.3 * scale, egui::Color32::WHITE);
        match icon {
            VolumeTargetIcon::Named(name) if name.eq_ignore_ascii_case("headphones") => {
                painter.circle_stroke(rect.center(), 15.0 * scale, stroke);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.left() + 5.0 * scale, rect.center().y),
                        egui::pos2(rect.left() + 13.0 * scale, rect.bottom() - 5.0 * scale),
                    ),
                    3.0 * scale,
                    egui::Color32::WHITE,
                );
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.right() - 13.0 * scale, rect.center().y),
                        egui::pos2(rect.right() - 5.0 * scale, rect.bottom() - 5.0 * scale),
                    ),
                    3.0 * scale,
                    egui::Color32::WHITE,
                );
            }
            VolumeTargetIcon::Named(name) if name.eq_ignore_ascii_case("group") => {
                painter.circle_filled(
                    rect.center() + egui::vec2(0.0, -8.0) * scale,
                    6.0 * scale,
                    self.accent,
                );
                painter.circle_filled(
                    rect.center() + egui::vec2(-12.0, -3.0) * scale,
                    4.5 * scale,
                    stroke.color,
                );
                painter.circle_filled(
                    rect.center() + egui::vec2(12.0, -3.0) * scale,
                    4.5 * scale,
                    stroke.color,
                );
                painter.rect_filled(
                    egui::Rect::from_center_size(
                        rect.center() + egui::vec2(0.0, 10.0) * scale,
                        egui::vec2(34.0, 13.0) * scale,
                    ),
                    7.0 * scale,
                    self.accent,
                );
            }
            VolumeTargetIcon::Named(name) if name.eq_ignore_ascii_case("stream") => {
                painter.circle_filled(rect.center(), 4.0 * scale, self.accent);
                painter.circle_stroke(rect.center(), 11.0 * scale, stroke);
                painter.circle_stroke(rect.center(), 19.0 * scale, stroke);
            }
            _ => {
                painter.image(
                    self.app_icon_texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_commands(ctx);
        self.receive_latest(ctx);
        self.hide_if_idle();
        ctx.set_visuals(egui::Visuals::dark());

        let settings_visible = self.render_settings(ctx);
        if !settings_visible {
            if let Some(update) = &self.update {
                let font_family = install_overlay_font(ctx, &update.font_family);
                let scale = effective_layout_scale(update.scale);
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgba_unmultiplied(
                                37,
                                37,
                                37,
                                OVERLAY_BACKGROUND_ALPHA,
                            ))
                            .corner_radius((14.0 * scale) as u8)
                            .inner_margin(egui::Margin::same((16.0 * scale) as i8)),
                    )
                    .show(ctx, |ui| {
                        let painter = ui.painter();
                        let icon_top = if update.show_target_name { 44.0 } else { 18.0 } * scale;
                        let icon_slot = egui::Rect::from_min_size(
                            egui::pos2(16.0 * scale, icon_top),
                            egui::vec2(48.0, 48.0) * scale,
                        );
                        let icon_rect = egui::Rect::from_center_size(
                            icon_slot.center(),
                            icon_slot.size() * update.icon_scale.clamp(0.5, 1.5),
                        );
                        self.render_icon(painter, icon_rect, &update.icon);
                        if update.is_muted {
                            draw_muted_overlay(painter, icon_rect);
                        }

                        if update.show_target_name {
                            painter.text(
                                egui::pos2(80.0, 16.0) * scale,
                                egui::Align2::LEFT_TOP,
                                &update.target_name,
                                egui::FontId::new(
                                    16.0 * scale * effective_text_scale(update.font_scale),
                                    font_family.clone(),
                                ),
                                egui::Color32::WHITE,
                            );
                        }

                        let bar_top = if update.show_target_name { 66.0 } else { 38.0 } * scale;
                        let bar_rect = egui::Rect::from_min_size(
                            egui::pos2(80.0 * scale, bar_top),
                            egui::vec2(270.0, 8.0) * scale,
                        );
                        painter.rect_filled(bar_rect, 4.0 * scale, egui::Color32::from_gray(70));
                        let percent = update.percent.clamp(0.0, 100.0);
                        let fill_rect = egui::Rect::from_min_size(
                            bar_rect.min,
                            egui::vec2(bar_rect.width() * percent / 100.0, bar_rect.height()),
                        );
                        painter.rect_filled(fill_rect, 4.0 * scale, self.accent);
                        painter.text(
                            egui::pos2(368.0 * scale, bar_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("{percent:.0}"),
                            egui::FontId::new(
                                16.0 * scale * effective_text_scale(update.font_scale),
                                font_family.clone(),
                            ),
                            egui::Color32::WHITE,
                        );
                    });
            }
        }

        if self.visible && !settings_visible {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if settings_visible {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

fn preview_update(config: &Config) -> VolumeUpdate {
    let (target_name, icon) = match config.target_mix {
        MixTarget::Monitor => ("Personal Mix", "headphones"),
        MixTarget::Stream => ("Stream Mix", "stream"),
        MixTarget::Chat => ("Chat Mix", "group"),
    };
    VolumeUpdate {
        percent: 65.0,
        target_name: target_name.to_owned(),
        icon: VolumeTargetIcon::Named(icon.to_owned()),
        is_muted: false,
        show_target_name: config.overlay_show_target_name,
        scale: config.overlay_scale,
        icon_scale: config.overlay_icon_scale,
        font_scale: config.overlay_font_scale,
        font_family: config.overlay_font_family.clone(),
        hide_delay_seconds: config.overlay_hide_delay_seconds,
    }
}

fn effective_layout_scale(value: f32) -> f32 {
    value.clamp(0.5, 2.0) * DEFAULT_LAYOUT_SCALE
}

fn effective_text_scale(value: f32) -> f32 {
    value.clamp(0.5, 1.5) * DEFAULT_TEXT_SCALE
}

fn overlay_logical_size(show_name: bool, scale: f32) -> [f32; 2] {
    let height = if show_name {
        OVERLAY_HEIGHT_WITH_NAME
    } else {
        OVERLAY_HEIGHT_WITHOUT_NAME
    };
    let scale = effective_layout_scale(scale);
    [OVERLAY_WIDTH * scale, height * scale]
}

fn render_preview_overlay(
    ctx: &egui::Context,
    update: &VolumeUpdate,
    accent: egui::Color32,
    font_family: &egui::FontFamily,
) {
    let scale = effective_layout_scale(update.scale);
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(
                    37,
                    37,
                    37,
                    PREVIEW_BACKGROUND_ALPHA,
                ))
                .corner_radius((14.0 * scale) as u8)
                .inner_margin(egui::Margin::same((16.0 * scale) as i8)),
        )
        .show(ctx, |ui| {
            let painter = ui.painter();
            let icon_top = if update.show_target_name { 44.0 } else { 18.0 } * scale;
            let icon_slot = egui::Rect::from_min_size(
                egui::pos2(16.0 * scale, icon_top),
                egui::vec2(48.0, 48.0) * scale,
            );
            let icon_rect = egui::Rect::from_center_size(
                icon_slot.center(),
                icon_slot.size() * update.icon_scale.clamp(0.5, 1.5),
            );
            draw_preview_icon(painter, icon_rect, &update.icon, accent);
            if update.is_muted {
                draw_muted_overlay(painter, icon_rect);
            }

            if update.show_target_name {
                painter.text(
                    egui::pos2(80.0, 16.0) * scale,
                    egui::Align2::LEFT_TOP,
                    &update.target_name,
                    egui::FontId::new(
                        16.0 * scale * effective_text_scale(update.font_scale),
                        font_family.clone(),
                    ),
                    egui::Color32::WHITE,
                );
            }

            let bar_top = if update.show_target_name { 66.0 } else { 38.0 } * scale;
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(80.0 * scale, bar_top),
                egui::vec2(270.0, 8.0) * scale,
            );
            painter.rect_filled(bar_rect, 4.0 * scale, egui::Color32::from_gray(70));
            let fill_rect = egui::Rect::from_min_size(
                bar_rect.min,
                egui::vec2(bar_rect.width() * update.percent / 100.0, bar_rect.height()),
            );
            painter.rect_filled(fill_rect, 4.0 * scale, accent);
            painter.text(
                egui::pos2(368.0 * scale, bar_rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{:.0}", update.percent),
                egui::FontId::new(
                    16.0 * scale * effective_text_scale(update.font_scale),
                    font_family.clone(),
                ),
                egui::Color32::WHITE,
            );
        });
}

fn draw_preview_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: &VolumeTargetIcon,
    accent: egui::Color32,
) {
    let scale = rect.width() / 48.0;
    let stroke = egui::Stroke::new(2.3 * scale, egui::Color32::WHITE);
    match icon {
        VolumeTargetIcon::Named(name) if name.eq_ignore_ascii_case("stream") => {
            painter.circle_filled(rect.center(), 4.0 * scale, accent);
            painter.circle_stroke(rect.center(), 11.0 * scale, stroke);
            painter.circle_stroke(rect.center(), 19.0 * scale, stroke);
        }
        VolumeTargetIcon::Named(name) if name.eq_ignore_ascii_case("group") => {
            painter.circle_filled(
                rect.center() + egui::vec2(0.0, -8.0) * scale,
                6.0 * scale,
                accent,
            );
            painter.circle_filled(
                rect.center() + egui::vec2(-12.0, -3.0) * scale,
                4.5 * scale,
                stroke.color,
            );
            painter.circle_filled(
                rect.center() + egui::vec2(12.0, -3.0) * scale,
                4.5 * scale,
                stroke.color,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(
                    rect.center() + egui::vec2(0.0, 10.0) * scale,
                    egui::vec2(34.0, 13.0) * scale,
                ),
                7.0 * scale,
                accent,
            );
        }
        _ => {
            painter.circle_stroke(rect.center(), 15.0 * scale, stroke);
            for (left, right) in [(5.0, 13.0), (35.0, 43.0)] {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.left() + left * scale, rect.center().y),
                        egui::pos2(rect.left() + right * scale, rect.bottom() - 5.0 * scale),
                    ),
                    3.0 * scale,
                    egui::Color32::WHITE,
                );
            }
        }
    }
}

fn draw_muted_overlay(painter: &egui::Painter, icon_rect: egui::Rect) {
    let size = icon_rect.width().min(icon_rect.height()) * 0.62;
    let rect = egui::Rect::from_center_size(icon_rect.center(), egui::vec2(size, size));
    let red = egui::Color32::from_rgb(244, 67, 54);
    let outline = egui::Stroke::new(
        (size * 0.20).clamp(4.0, 8.0),
        egui::Color32::from_rgb(37, 37, 37),
    );
    let stroke = egui::Stroke::new((size * 0.13).clamp(3.0, 6.0), red);
    for line in [
        [rect.left_top(), rect.right_bottom()],
        [rect.right_top(), rect.left_bottom()],
    ] {
        painter.line_segment(line, outline);
        painter.line_segment(line, stroke);
    }
}

fn install_overlay_font(ctx: &egui::Context, requested_family: &str) -> egui::FontFamily {
    let Some((_display_name, bytes)) = crate::tray::overlay_font_bytes(requested_family) else {
        return egui::FontFamily::Proportional;
    };
    let family_name = format!("wavekeys-overlay-{requested_family}");
    let mut fonts = OVERLAY_FONTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("overlay font cache poisoned");
    let is_new = !fonts.contains_key(&family_name);
    if is_new {
        fonts.insert(family_name.clone(), bytes);
        let mut definitions = egui::FontDefinitions::default();
        for (loaded_family, data) in fonts.iter() {
            let font_key = format!("{loaded_family}-data");
            definitions.font_data.insert(
                font_key.clone(),
                Arc::new(egui::FontData::from_owned(data.clone())),
            );
            definitions.families.insert(
                egui::FontFamily::Name(loaded_family.clone().into()),
                vec![font_key],
            );
        }
        ctx.set_fonts(definitions);
        ctx.request_repaint();
        return egui::FontFamily::Proportional;
    }
    egui::FontFamily::Name(family_name.into())
}

fn channel_texture(ctx: &egui::Context, icon: &VolumeTargetIcon) -> Option<egui::TextureHandle> {
    let VolumeTargetIcon::PngBase64(encoded) = icon else {
        return None;
    };
    let bytes = STANDARD.decode(encoded).ok()?;
    let rgba = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ctx.load_texture(
        "volume-overlay-channel-icon",
        egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
        egui::TextureOptions::LINEAR,
    ))
}

fn configure_overlay_window(window: HWND) {
    ensure_overlay_style(window);
    unsafe {
        let previous = SetWindowLongPtrW(
            window,
            GWLP_WNDPROC,
            overlay_window_proc as *const () as usize as isize,
        );
        let _ = ORIGINAL_WINDOW_PROC.set(previous);
    }
}

unsafe extern "system" fn overlay_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if SETTINGS_INTERACTIVE.load(Ordering::Relaxed)
        && message == WM_NCLBUTTONDOWN
        && wparam.0 == HTCAPTION as usize
    {
        let _ = unsafe { ReleaseCapture() };
        return unsafe {
            SendMessageW(
                window,
                WM_SYSCOMMAND,
                Some(WPARAM((SC_MOVE | HTCAPTION) as usize)),
                Some(LPARAM(0)),
            )
        };
    }
    if SETTINGS_INTERACTIVE.load(Ordering::Relaxed) && message == WM_NCHITTEST {
        let packed = lparam.0 as u32;
        let pointer_x = (packed as u16 as i16) as i32;
        let pointer_y = ((packed >> 16) as u16 as i16) as i32;
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(window, &mut rect) }.is_ok() {
            let dpi_scale = unsafe { GetDpiForWindow(window) } as f32 / 96.0;
            let titlebar_bottom = rect.top + (42.0 * dpi_scale).round() as i32;
            let close_button_left = rect.right - (52.0 * dpi_scale).round() as i32;
            if pointer_y >= rect.top
                && pointer_y < titlebar_bottom
                && pointer_x >= rect.left
                && pointer_x < close_button_left
            {
                return LRESULT(HTCAPTION as isize);
            }
        }
    }
    if !SETTINGS_INTERACTIVE.load(Ordering::Relaxed) && message == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    if !SETTINGS_INTERACTIVE.load(Ordering::Relaxed) && message == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    let previous = ORIGINAL_WINDOW_PROC.get().copied().unwrap_or_default();
    let procedure: WNDPROC = if previous == 0 {
        None
    } else {
        Some(unsafe {
            std::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(previous)
        })
    };
    unsafe { CallWindowProcW(procedure, window, message, wparam, lparam) }
}

fn ensure_overlay_style(window: HWND) {
    unsafe {
        let extended_style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let overlay_style = WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0;
        SetWindowLongPtrW(window, GWL_EXSTYLE, extended_style | overlay_style as isize);
    }
}

fn set_settings_interactive(window: HWND, interactive: bool) {
    SETTINGS_INTERACTIVE.store(interactive, Ordering::Relaxed);
    set_clickthrough(window, false);
    unsafe {
        let extended_style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let next_style = if interactive {
            extended_style & !(WS_EX_NOACTIVATE.0 as isize)
        } else {
            extended_style | WS_EX_NOACTIVATE.0 as isize
        };
        SetWindowLongPtrW(window, GWL_EXSTYLE, next_style);
    }
}

fn start_window_mode_worker(window: HWND, receiver: Receiver<WindowMode>) {
    let window_address = window.0 as usize;
    let _ = std::thread::Builder::new()
        .name("wavekeys-overlay-window".to_owned())
        .spawn(move || {
            let window = HWND(window_address as *mut _);
            let mut startup_hide_pending = true;
            let mut clickthrough_pending = false;
            loop {
                let action = if startup_hide_pending {
                    receiver.recv_timeout(Duration::from_millis(100))
                } else if clickthrough_pending {
                    receiver.recv_timeout(Duration::from_millis(50))
                } else {
                    match receiver.recv() {
                        Ok(action) => Ok(action),
                        Err(_) => return,
                    }
                };
                match action {
                    Ok(WindowMode::ArmClickthrough) => {
                        startup_hide_pending = false;
                        clickthrough_pending = true;
                        set_clickthrough(window, false);
                    }
                    Ok(WindowMode::DisableClickthrough) => {
                        startup_hide_pending = false;
                        clickthrough_pending = false;
                        set_clickthrough(window, false);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) if startup_hide_pending => {
                        unsafe {
                            let _ = ShowWindow(window, SW_HIDE);
                        }
                        startup_hide_pending = false;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        set_clickthrough(window, true);
                        clickthrough_pending = false;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        });
}

fn set_clickthrough(window: HWND, enabled: bool) {
    unsafe {
        let extended_style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let next_style = if enabled {
            extended_style | WS_EX_TRANSPARENT.0 as isize
        } else {
            extended_style & !(WS_EX_TRANSPARENT.0 as isize)
        };
        SetWindowLongPtrW(window, GWL_EXSTYLE, next_style);
    }
}

fn show_settings_window(window: HWND, pixels_per_point: f32, monitor: HMONITOR) {
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let width = (440.0 * pixels_per_point).round() as i32;
        let height = (634.0 * pixels_per_point).round() as i32;
        let x = info.rcWork.left + (info.rcWork.right - info.rcWork.left - width) / 2;
        let y = info.rcWork.top + (info.rcWork.bottom - info.rcWork.top - height) / 2;
        set_settings_interactive(window, true);
        configure_settings_mica(window);
        let _ = SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_SHOWWINDOW,
        );
        let _ = SetForegroundWindow(window);
    }
}

fn configure_settings_mica(window: HWND) {
    configure_backdrop(window, DWMSBT_MAINWINDOW);
}

fn configure_overlay_backdrop(window: HWND) {
    configure_backdrop(window, DWMSBT_TABBEDWINDOW);
}

fn disable_backdrop(window: HWND) {
    configure_backdrop(window, DWMSBT_NONE);
}

fn configure_backdrop(window: HWND, backdrop: DWM_SYSTEMBACKDROP_TYPE) {
    unsafe {
        let dark_mode = 1u32;
        let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        for (attribute, value, size) in [
            (
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_mode as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&dark_mode) as u32,
            ),
            (
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&backdrop) as u32,
            ),
            (
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&corner) as u32,
            ),
        ] {
            if let Err(error) = DwmSetWindowAttribute(window, attribute, value, size) {
                tracing::warn!(%error, "unable to configure volume overlay backdrop");
            }
        }
    }
}

fn position_and_show(window: HWND, pixels_per_point: f32, show_name: bool, scale: f32) {
    unsafe {
        ensure_overlay_style(window);
        let mut cursor = POINT::default();
        let monitor = if GetCursorPos(&mut cursor).is_ok() {
            MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
        } else {
            MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST)
        };
        position_and_show_on_monitor(window, pixels_per_point, show_name, scale, Some(monitor));
    }
}

fn position_and_show_on_monitor(
    window: HWND,
    pixels_per_point: f32,
    show_name: bool,
    scale: f32,
    monitor: Option<HMONITOR>,
) {
    unsafe {
        ensure_overlay_style(window);
        let monitor = monitor
            .unwrap_or_else(|| MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST));
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            tracing::warn!("unable to determine the work area for the volume overlay");
            return;
        }
        let scale = effective_layout_scale(scale);
        let width = (OVERLAY_WIDTH * scale * pixels_per_point).round() as i32;
        let logical_height = if show_name {
            OVERLAY_HEIGHT_WITH_NAME
        } else {
            OVERLAY_HEIGHT_WITHOUT_NAME
        };
        let height = (logical_height * scale * pixels_per_point).round() as i32;
        let gap = (TASKBAR_GAP * pixels_per_point).round() as i32;
        let x = info.rcWork.left + (info.rcWork.right - info.rcWork.left - width) / 2;
        let y = info.rcWork.bottom - height - gap;
        if let Err(error) = SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        ) {
            tracing::warn!(%error, "unable to position the volume overlay");
        }
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
    }
}

fn windows_accent() -> egui::Color32 {
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\DWM",
            "/v",
            "ColorizationColor",
        ])
        .output();
    output
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| {
            text.split_whitespace()
                .last()
                .and_then(|hex| u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        })
        .map(|argb| {
            egui::Color32::from_rgb(
                ((argb >> 16) & 0xff) as u8,
                ((argb >> 8) & 0xff) as u8,
                (argb & 0xff) as u8,
            )
        })
        .unwrap_or(egui::Color32::from_rgb(0, 120, 212))
}

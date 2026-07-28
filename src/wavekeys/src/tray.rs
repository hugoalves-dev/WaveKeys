// Copyright (c) Modding Forge

use crate::{
    configure_autostart,
    overlay::{OverlayCommand, OverlayCommandSender},
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};
use wavekeys_core::{Config, ControlScope, MixTarget};

fn settings_background() -> egui::Color32 {
    egui::Color32::TRANSPARENT
}

/// Owns the native tray icon and opens the settings window on demand.
pub fn run(overlay_commands: OverlayCommandSender) -> Result<(), String> {
    let menu = Menu::new();
    let settings = MenuItem::new("Settings", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append(&settings).map_err(|error| error.to_string())?;
    menu.append(&quit).map_err(|error| error.to_string())?;
    let icon = TrayIconBuilder::new()
        .with_tooltip("WaveKeys")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(crate::icon::tray_icon()?)
        .build()
        .map_err(|error| error.to_string())?;
    show_startup_notification(&icon);
    let _keep_alive = icon;
    let mut ignore_tray_events_until = None;
    loop {
        pump_messages();
        let mut show_settings = false;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == settings.id() {
                show_settings = true;
            }
            if event.id == quit.id() {
                overlay_commands.send(OverlayCommand::Quit);
                return Ok(());
            }
        }

        if ignore_tray_events_until.is_some_and(|deadline| Instant::now() < deadline) {
            while TrayIconEvent::receiver().try_recv().is_ok() {}
        } else {
            ignore_tray_events_until = None;
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    show_settings = true;
                }
            }
        }

        if show_settings {
            overlay_commands.send(OverlayCommand::OpenSettings);
            // Windows emits another click/double-click event for the gesture that
            // opened the window. Discard it instead of immediately reopening.
            ignore_tray_events_until = Some(Instant::now() + Duration::from_millis(500));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn show_startup_notification(icon: &tray_icon::TrayIcon) {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HINSTANCE, HWND},
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Shell::{
                    Shell_NotifyIconW, NIF_INFO, NIIF_LARGE_ICON, NIIF_USER, NIM_MODIFY,
                    NOTIFYICONDATAW, NOTIFYICONDATAW_0,
                },
                WindowsAndMessaging::{
                    DestroyIcon, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR,
                },
            },
        },
    };

    // Win32's MAKEINTRESOURCE convention represents resource ID 1 as pointer value 1.
    #[allow(clippy::manual_dangling_ptr)]
    let app_icon_resource = PCWSTR(1usize as *const u16);
    let balloon_icon = unsafe {
        GetModuleHandleW(None).ok().and_then(|module| {
            LoadImageW(
                Some(HINSTANCE(module.0)),
                app_icon_resource,
                IMAGE_ICON,
                64,
                64,
                LR_DEFAULTCOLOR,
            )
            .ok()
            .map(|handle| HICON(handle.0))
        })
    };
    let mut notification = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(icon.window_handle()),
        uFlags: NIF_INFO,
        Anonymous: NOTIFYICONDATAW_0 { uTimeout: 5_000 },
        dwInfoFlags: NIIF_USER | NIIF_LARGE_ICON,
        hBalloonIcon: balloon_icon.unwrap_or_default(),
        ..Default::default()
    };
    copy_utf16("WaveInput is active now", &mut notification.szInfo);
    copy_utf16("WaveInput", &mut notification.szInfoTitle);

    // tray-icon does not expose its private shell identifier. Probe the small
    // process-local ID range for this HWND; only the matching pair can succeed.
    for shell_id in 1..=32 {
        notification.uID = shell_id;
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &notification) }.as_bool() {
            if let Some(icon) = balloon_icon {
                let _ = unsafe { DestroyIcon(icon) };
            }
            return;
        }
    }
    if let Some(icon) = balloon_icon {
        let _ = unsafe { DestroyIcon(icon) };
    }
    tracing::warn!("unable to show the startup notification");
}

#[cfg(not(windows))]
fn show_startup_notification(_: &tray_icon::TrayIcon) {}

fn copy_utf16<const N: usize>(source: &str, destination: &mut [u16; N]) {
    for (target, value) in destination
        .iter_mut()
        .take(N.saturating_sub(1))
        .zip(source.encode_utf16())
    {
        *target = value;
    }
}

fn pump_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

pub(crate) struct SettingsApp {
    config: Config,
    autostart: bool,
    accent: egui::Color32,
    close_requested: bool,
}

impl SettingsApp {
    pub(crate) fn new() -> Self {
        let config = Config::load().unwrap_or_default();
        let autostart = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "WaveKeys",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        Self {
            config,
            autostart,
            accent: windows_accent(),
            close_requested: false,
        }
    }

    pub(crate) fn take_close_requested(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    fn persist(&mut self) {
        if let Err(error) = self.config.save() {
            tracing::warn!(%error, "unable to save settings");
        }
        if let Err(error) =
            configure_autostart(Some(if self.autostart { "enable" } else { "disable" }))
        {
            tracing::warn!(%error, "unable to update autostart");
        }
    }
}

impl SettingsApp {
    pub(crate) fn show(&mut self, ctx: &egui::Context) {
        egui_extras::install_image_loaders(ctx);
        ctx.set_visuals(egui::Visuals::dark());
        ctx.style_mut(|style| {
            style.visuals.selection.bg_fill = self.accent;
            style.visuals.selection.stroke = egui::Stroke::new(1.5_f32, self.accent);

            style.visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(
                self.accent.r(),
                self.accent.g(),
                self.accent.b(),
                48,
            );
            style.visuals.widgets.hovered.bg_fill = self.accent.gamma_multiply(0.55);
            style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.25_f32, self.accent);
            style.visuals.widgets.hovered.fg_stroke =
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE);

            style.visuals.widgets.active.weak_bg_fill = self.accent.gamma_multiply(0.45);
            style.visuals.widgets.active.bg_fill = self.accent;
            style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5_f32, self.accent);
            style.visuals.widgets.active.fg_stroke =
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE);
            style.visuals.widgets.open = style.visuals.widgets.active;
            style.visuals.widgets.inactive.fg_stroke =
                egui::Stroke::new(1.0_f32, egui::Color32::LIGHT_GRAY);
        });
        egui::TopBottomPanel::top("titlebar")
            .frame(
                egui::Frame::NONE
                    .fill(settings_background())
                    .inner_margin(0.0),
            )
            .exact_height(42.0)
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                let icon_rect = egui::Rect::from_center_size(
                    egui::pos2(bar.left() + 18.0, bar.center().y),
                    egui::vec2(22.0, 22.0),
                );
                ui.put(
                    icon_rect,
                    egui::Image::new(egui::include_image!("../assets/waveinput.svg"))
                        .fit_to_exact_size(icon_rect.size()),
                );

                let close_rect = egui::Rect::from_center_size(
                    egui::pos2(bar.right() - 16.0, bar.center().y),
                    egui::vec2(28.0, 28.0),
                );
                if ui
                    .put(
                        close_rect,
                        egui::Button::image(
                            egui::Image::new(egui::include_image!("../assets/close.svg"))
                                .fit_to_exact_size(egui::vec2(17.0, 17.0)),
                        )
                        .frame(false)
                        .image_tint_follows_text_color(true),
                    )
                    .clicked()
                {
                    self.close_requested = true;
                }

                let title_rect = egui::Rect::from_center_size(
                    bar.center(),
                    egui::vec2(bar.width() - 80.0, bar.height()),
                );
                ui.painter().text(
                    bar.center(),
                    egui::Align2::CENTER_CENTER,
                    "WaveKeys",
                    egui::FontId::proportional(16.0),
                    egui::Color32::WHITE,
                );
                let title = ui.interact(
                    title_rect,
                    egui::Id::new("settings-title-drag"),
                    egui::Sense::click_and_drag(),
                );
                let _ = title;
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(settings_background())
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Media controls").size(20.0).strong());
                ui.label(
                    egui::RichText::new("Control the focused app or an entire Wave Link mix.")
                        .color(egui::Color32::GRAY),
                );
                ui.add_space(16.0);
                settings_group(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label("Step size");
                    let step_changed =
                        percent_slider(ui, &mut self.config.step_percent, 0.1..=100.0, 1.0)
                            .on_hover_text("Volume change per key press")
                            .changed();
                    if step_changed {
                        self.persist();
                    }
                });
                ui.add_space(8.0);
                settings_group(ui, |ui| {
                    ui.label("Target mix");
                    egui::ComboBox::from_id_salt("mix")
                        .width(128.0)
                        .selected_text(
                            egui::RichText::new(match self.config.target_mix {
                                MixTarget::Monitor => "Personal Mix",
                                MixTarget::Stream => "Stream Mix",
                                MixTarget::Chat => "Chat Mix",
                            })
                            .color(egui::Color32::WHITE),
                        )
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_value(
                                    &mut self.config.target_mix,
                                    MixTarget::Monitor,
                                    "Personal Mix",
                                )
                                .on_hover_text("Local Personal Mix")
                                .changed()
                            {
                                self.persist();
                            }
                            if ui
                                .selectable_value(
                                    &mut self.config.target_mix,
                                    MixTarget::Chat,
                                    "Chat Mix",
                                )
                                .on_hover_text("Wave Link Chat Mix")
                                .changed()
                            {
                                self.persist();
                            }
                            if ui
                                .selectable_value(
                                    &mut self.config.target_mix,
                                    MixTarget::Stream,
                                    "Stream Mix",
                                )
                                .on_hover_text("Wave Link Stream Mix")
                                .changed()
                            {
                                self.persist();
                            }
                        });
                    ui.add_space(5.0);
                    ui.label("Control scope");
                    egui::ComboBox::from_id_salt("scope")
                        .width(128.0)
                        .selected_text(
                            egui::RichText::new(match self.config.control_scope {
                                ControlScope::Focus => "Focused app",
                                ControlScope::Global => "Global",
                            })
                            .color(egui::Color32::WHITE),
                        )
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_value(
                                    &mut self.config.control_scope,
                                    ControlScope::Focus,
                                    "Focused app",
                                )
                                .on_hover_text(
                                    "Control only the channel assigned to the focused app",
                                )
                                .changed()
                            {
                                self.persist();
                            }
                            if ui
                                .selectable_value(
                                    &mut self.config.control_scope,
                                    ControlScope::Global,
                                    "Global",
                                )
                                .on_hover_text(
                                    "Control the entire selected mix, independent of focus",
                                )
                                .changed()
                            {
                                self.persist();
                            }
                        });
                });
                ui.add_space(8.0);
                settings_group(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label("Volume overlay");
                    let overlay_changed = accent_checkbox(
                        ui,
                        self.accent,
                        &mut self.config.overlay_enabled,
                        "Show volume overlay",
                    )
                    .on_hover_text("Show the confirmed Wave Link level after a volume change")
                    .changed();
                    let name_changed = ui
                        .add_enabled_ui(self.config.overlay_enabled, |ui| {
                            accent_checkbox(
                                ui,
                                self.accent,
                                &mut self.config.overlay_show_target_name,
                                "Show channel or mix name",
                            )
                        })
                        .inner
                        .on_hover_text("Display the affected target above the level")
                        .changed();
                    let scale_changed = ui
                        .add_enabled_ui(self.config.overlay_enabled, |ui| {
                            ui.label("Scale");
                            percent_slider(ui, &mut self.config.overlay_scale, 0.5..=2.0, 100.0)
                        })
                        .inner
                        .on_hover_text("Resize the overlay from 50% to 200%")
                        .changed();
                    let icon_scale_changed = ui
                        .add_enabled_ui(self.config.overlay_enabled, |ui| {
                            ui.label("Icon size");
                            percent_slider(
                                ui,
                                &mut self.config.overlay_icon_scale,
                                0.5..=1.5,
                                100.0,
                            )
                        })
                        .inner
                        .on_hover_text("Resize only the channel or mix icon")
                        .changed();
                    let font_scale_changed = ui
                        .add_enabled_ui(self.config.overlay_enabled, |ui| {
                            ui.label("Text size");
                            percent_slider(
                                ui,
                                &mut self.config.overlay_font_scale,
                                0.5..=1.5,
                                100.0,
                            )
                        })
                        .inner
                        .on_hover_text("Resize the overlay name and percentage")
                        .changed();
                    let previous_font = self.config.overlay_font_family.clone();
                    ui.add_enabled_ui(self.config.overlay_enabled, |ui| {
                        ui.label("Font");
                        egui::ComboBox::from_id_salt("overlay-font")
                            .width(168.0)
                            .selected_text(
                                egui::RichText::new(font_display_name(
                                    &self.config.overlay_font_family,
                                ))
                                .color(egui::Color32::WHITE),
                            )
                            .height(260.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.config.overlay_font_family,
                                    "system".to_owned(),
                                    "Windows default",
                                );
                                for font in installed_fonts() {
                                    ui.selectable_value(
                                        &mut self.config.overlay_font_family,
                                        font.name.clone(),
                                        &font.name,
                                    );
                                }
                            });
                    });
                    let font_changed = previous_font != self.config.overlay_font_family;
                    let delay_changed = ui
                        .add_enabled_ui(self.config.overlay_enabled, |ui| {
                            ui.label("Hide delay");
                            ui.add(
                                egui::DragValue::new(&mut self.config.overlay_hide_delay_seconds)
                                    .range(0.1..=10.0)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" s"),
                            )
                        })
                        .inner
                        .on_hover_text("Seconds after the last volume change; decimals are allowed")
                        .changed();
                    if overlay_changed
                        || name_changed
                        || scale_changed
                        || icon_scale_changed
                        || font_scale_changed
                        || font_changed
                        || delay_changed
                    {
                        self.persist();
                    }
                });
                ui.add_space(8.0);
                let footer = ui.horizontal(|ui| {
                    let changed =
                        accent_checkbox(ui, self.accent, &mut self.autostart, "Start with Windows")
                            .on_hover_text("Start automatically after signing in to Windows")
                            .changed();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if footer_link_button(
                            ui,
                            self.accent,
                            egui::include_image!("../assets/github.svg"),
                            "WaveKeys on GitHub",
                            18.0,
                        ) {
                            open_url("https://github.com/Wuerfelhusten/WaveKeys");
                        }
                        if footer_link_button(
                            ui,
                            self.accent,
                            egui::include_image!("../assets/kofi.svg"),
                            "Ko-fi: wuerfelhusten",
                            19.8,
                        ) {
                            open_url("https://ko-fi.com/wuerfelhusten");
                        }
                    });
                    changed
                });
                if footer.inner {
                    self.persist();
                }
            });
    }
}

fn settings_group<R>(
    ui: &mut egui::Ui,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::group(ui.style())
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(10))
        .show(ui, content)
}

fn footer_link_button(
    ui: &mut egui::Ui,
    accent: egui::Color32,
    source: egui::ImageSource<'static>,
    tooltip: &str,
    icon_size: f32,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    if response.hovered() {
        let fill = if response.is_pointer_button_down_on() {
            accent.gamma_multiply(0.8)
        } else {
            accent
        };
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    let image_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(icon_size, icon_size));
    egui::Image::new(source)
        .fit_to_exact_size(image_rect.size())
        .tint(egui::Color32::WHITE)
        .paint_at(ui, image_rect);
    response.on_hover_text(tooltip).clicked()
}

fn accent_checkbox(
    ui: &mut egui::Ui,
    accent: egui::Color32,
    value: &mut bool,
    label: &str,
) -> egui::Response {
    let row = ui.horizontal(|ui| {
        let checkbox = ui
            .scope(|ui| {
                let widgets = &mut ui.style_mut().visuals.widgets;
                widgets.inactive.fg_stroke = egui::Stroke::new(2.0_f32, accent);
                widgets.hovered.fg_stroke = egui::Stroke::new(2.0_f32, accent);
                widgets.active.fg_stroke = egui::Stroke::new(2.0_f32, egui::Color32::WHITE);
                ui.add(egui::Checkbox::without_text(value))
            })
            .inner;
        let label = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
        if label.clicked() {
            *value = !*value;
        }
        let changed = checkbox.changed() || label.clicked();
        (checkbox.union(label), changed)
    });
    let (mut response, changed) = row.inner;
    if changed {
        response.mark_changed();
    }
    response
}

fn percent_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    display_multiplier: f32,
) -> egui::Response {
    let minimum = *range.start();
    let maximum = *range.end();
    let row = ui.horizontal(|ui| {
        let slider = ui.add(
            egui::Slider::new(value, range)
                .show_value(false)
                .trailing_fill(true),
        );
        let mut displayed = *value * display_multiplier;
        let input = ui.add(
            egui::DragValue::new(&mut displayed)
                .range((minimum * display_multiplier)..=(maximum * display_multiplier))
                .speed(0.1)
                .max_decimals(1)
                .suffix(" %"),
        );
        if input.changed() {
            *value = (displayed / display_multiplier).clamp(minimum, maximum);
        }
        slider.changed() || input.changed()
    });
    let mut response = row.response;
    if row.inner {
        response.mark_changed();
    }
    response
}

pub(crate) struct InstalledFont {
    pub(crate) name: String,
    path: PathBuf,
}

static INSTALLED_FONTS: OnceLock<Vec<InstalledFont>> = OnceLock::new();

pub(crate) fn installed_fonts() -> &'static [InstalledFont] {
    INSTALLED_FONTS.get_or_init(discover_installed_fonts)
}

pub(crate) fn overlay_font_bytes(family: &str) -> Option<(String, Vec<u8>)> {
    let font = if family == "system" {
        let windows_dir = std::env::var_os("WINDIR")?;
        return std::fs::read(PathBuf::from(windows_dir).join("Fonts").join("segoeui.ttf"))
            .ok()
            .map(|bytes| ("Windows default".to_owned(), bytes));
    } else {
        installed_fonts().iter().find(|font| font.name == family)?
    };
    std::fs::read(&font.path)
        .ok()
        .map(|bytes| (font.name.clone(), bytes))
}

fn font_display_name(family: &str) -> &str {
    if family == "system" {
        "Windows default"
    } else {
        family
    }
}

fn discover_installed_fonts() -> Vec<InstalledFont> {
    let mut fonts = Vec::new();
    for registry_key in [
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts",
        r"HKCU\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts",
    ] {
        let Ok(output) = std::process::Command::new("reg")
            .args(["query", registry_key])
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let Some((raw_name, raw_path)) = line
                .split_once("REG_SZ")
                .or_else(|| line.split_once("REG_EXPAND_SZ"))
            else {
                continue;
            };
            let name = raw_name
                .trim()
                .trim_end_matches(" (TrueType)")
                .trim_end_matches(" (OpenType)")
                .to_owned();
            let raw_path = raw_path.trim();
            let extension = std::path::Path::new(raw_path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if name.is_empty() || !matches!(extension.to_ascii_lowercase().as_str(), "ttf" | "otf")
            {
                continue;
            }
            let path = PathBuf::from(raw_path);
            let path = if path.is_absolute() {
                path
            } else {
                PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()))
                    .join("Fonts")
                    .join(path)
            };
            if path.exists() {
                fonts.push(InstalledFont { name, path });
            }
        }
    }
    fonts.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    fonts.dedup_by(|left, right| left.name.eq_ignore_ascii_case(&right.name));
    fonts
}

fn open_url(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
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
    let value = output
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| {
            text.split_whitespace()
                .last()
                .and_then(|hex| u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        });
    value
        .map(|argb| {
            egui::Color32::from_rgb(
                ((argb >> 16) & 0xff) as u8,
                ((argb >> 8) & 0xff) as u8,
                (argb & 0xff) as u8,
            )
        })
        .unwrap_or(egui::Color32::from_rgb(0, 120, 212))
}

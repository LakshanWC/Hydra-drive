#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{
    self, Color32, FontId, Id, Pos2, Rect, RichText, Rounding, Slider, Stroke, Vec2,
};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod helper;
use helper::file_move_helper::{create_syslink, link_direcories, restore_linked_files};
use helper::log_helper::read_uniqe_app_names;
use hydra_drive_core::{
    create_virtual_drive, format_bytes, list_drives, pool_folder_for, DriveInfo, VirtualDrive,
};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

// ── Color palette ──────────────────────────────────────────────────────
const ACCENT:   Color32 = Color32::from_rgb(100, 149, 237);
const ACCENT_D: Color32 = Color32::from_rgb(65, 108, 195);
const GREEN:    Color32 = Color32::from_rgb(75, 210, 120);
const AMBER:    Color32 = Color32::from_rgb(245, 188, 60);
const RED:      Color32 = Color32::from_rgb(235, 85, 85);
const PANEL:    Color32 = Color32::from_rgb(22, 23, 32);
const CARD:     Color32 = Color32::from_rgb(30, 31, 42);
const BORDER:   Color32 = Color32::from_rgb(48, 50, 66);
const INPUT_BG: Color32 = Color32::from_rgb(18, 18, 28);
const TEXT_H:   Color32 = Color32::from_rgb(235, 238, 248);
const TEXT_P:   Color32 = Color32::from_rgb(195, 200, 218);
const TEXT_DIM: Color32 = Color32::from_rgb(110, 115, 140);
const DANGER:   Color32 = Color32::from_rgb(160, 50, 50);

// ── State types ────────────────────────────────────────────────────────

#[derive(PartialEq, Default, Clone, Copy)]
enum Tab {
    #[default]
    Drive,
    Links,
}

#[derive(PartialEq, Default, Clone, Copy)]
enum Mode {
    #[default]
    LinkDir,
    LinkFile,
    Restore,
}

/// State of the elevated helper exe.
#[derive(PartialEq, Default, Clone)]
enum HelperState {
    Disabled,
    #[default]
    NotRunning,
    Starting,
    Running,
    Failed(String),
}

impl HelperState {
    /// Returns (color, short label, should_pulse).
    fn describe(&self) -> (Color32, &'static str, bool) {
        match self {
            Self::Disabled   => (TEXT_DIM, "Off",         false),
            Self::NotRunning => (TEXT_DIM, "Not running", false),
            Self::Starting   => (AMBER,   "Starting\u{2026}", true),
            Self::Running    => (GREEN,   "Running",     true),
            Self::Failed(_)  => (RED,     "Failed",      false),
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Self::Failed(r) => Some(r),
            _ => None,
        }
    }
}

/// One physical drive the user can contribute to the virtual drive.
struct DriveChoice {
    info: DriveInfo,
    enabled: bool,
    bytes: f64,     // how much of this drive goes to the pool
    folder: PathBuf, // root folder on this drive that holds the pool files
}

impl DriveChoice {
    fn new(info: DriveInfo) -> Self {
        let folder = pool_folder_for(&info.mount);
        Self {
            info,
            enabled: false,
            bytes: 0.0,
            folder,
        }
    }

    /// Largest quota the slider may offer: leave 1 GiB free on the disk.
    fn max_alloc(&self) -> f64 {
        (self.info.free as f64 - GIB).max(0.0)
    }

    fn usable(&self) -> bool {
        self.max_alloc() >= GIB
    }
}

// ── App ────────────────────────────────────────────────────────────────

struct App {
    tab: Tab,

    // virtual drive
    drive: Option<VirtualDrive>,
    drives: Vec<DriveChoice>,
    active_roots: Vec<(PathBuf, u64)>, // roots of the currently mounted drive
    admin_support: bool,
    admin_consent_given: bool,
    show_disclosure: bool,
    create_after_consent: bool,
    helper_state: HelperState,
    dummy_used_bytes: u64,

    // links
    mode: Mode,
    source: String,
    dest: String,
    app_name: String,
    app_names: Vec<String>,

    // status
    status: String,
    status_changed: Instant,

    // ui bookkeeping
    theme_applied: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            tab: Tab::default(),
            drive: None,
            drives: Vec::new(),
            active_roots: Vec::new(),
            admin_support: true,
            admin_consent_given: false,
            show_disclosure: false,
            create_after_consent: false,
            helper_state: HelperState::default(),
            dummy_used_bytes: 0,
            mode: Mode::default(),
            source: String::new(),
            dest: String::new(),
            app_name: String::new(),
            app_names: Vec::new(),
            status: "Ready".into(),
            status_changed: Instant::now(),
            theme_applied: false,
        }
    }
}

// ── Logic ──────────────────────────────────────────────────────────────

impl App {
    fn new() -> Self {
        let mut app = App::default();
        app.refresh_names();
        app.refresh_drives();
        app
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_changed = Instant::now();
    }

    fn refresh_names(&mut self) {
        self.app_names = read_uniqe_app_names().unwrap_or_default();
    }

    /// Re-scan physical drives, keeping the user's existing selections.
    fn refresh_drives(&mut self) {
        let mut fresh = Vec::new();
        for info in list_drives() {
            let mut choice = DriveChoice::new(info);
            if let Some(old) = self
                .drives
                .iter()
                .find(|d| d.info.mount == choice.info.mount)
            {
                choice.enabled = old.enabled;
                choice.bytes = old.bytes.min(choice.max_alloc());
                choice.folder = old.folder.clone();
            }
            fresh.push(choice);
        }
        self.drives = fresh;
    }

    /// Total size of the virtual drive = sum of the selected quotas.
    fn total_selected(&self) -> f64 {
        self.drives
            .iter()
            .filter(|d| d.enabled)
            .map(|d| d.bytes)
            .sum()
    }

    fn request_create(&mut self) {
        if self.drive.is_some() {
            self.set_status("V: is already mounted");
            return;
        }
        let picked: Vec<&DriveChoice> = self.drives.iter().filter(|d| d.enabled).collect();
        if picked.is_empty() {
            self.set_status("Select at least one drive");
            return;
        }
        if picked.iter().any(|d| d.bytes < GIB) {
            self.set_status("Every selected drive needs at least 1 GB");
            return;
        }
        if self.admin_support && !self.admin_consent_given {
            self.create_after_consent = true;
            self.show_disclosure = true;
            return;
        }
        self.do_create();
    }

    fn do_create(&mut self) {
        let roots: Vec<(PathBuf, u64)> = self
            .drives
            .iter()
            .filter(|d| d.enabled && d.bytes >= GIB)
            .map(|d| (d.folder.clone(), d.bytes as u64))
            .collect();
        
        match create_virtual_drive(roots.clone()) {
            Ok(d) => {
                self.drive = Some(d);
                hydra_drive_core::set_selected_roots(roots.clone());
                self.active_roots = roots;
                self.dummy_used_bytes = (4.5 * GIB) as u64;
                self.set_status("V: mounted successfully");
                if self.admin_support {
                    self.start_helper();
                } else {
                    self.helper_state = HelperState::Disabled;
                }
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    fn delete_drive(&mut self) {
        self.drive = None;
        self.active_roots.clear();
        hydra_drive_core::set_selected_roots(Vec::new());
        self.dummy_used_bytes = 0;
        self.helper_state = if self.admin_support {
            HelperState::NotRunning
        } else {
            HelperState::Disabled
        };
        self.refresh_drives();
        self.set_status("Virtual drive removed");
    }

    fn start_helper(&mut self) {
        // TODO: spawn the elevated helper exe

        hydra_drive_core::CREATE_PREVILAGE_VIRTUAL_DRIVE.store(true, Ordering::Relaxed);

        // TODO: ipc::start_hydra_drive_helper still takes ONE root. The helper has to
        // mount the same pool, so it needs the whole `self.active_roots` list.
        let Some((first_root, _)) = self.active_roots.first() else {
            self.helper_state = HelperState::Failed("no roots selected".into());
            return;
        };
        let total: u64 = self.active_roots.iter().map(|(_, q)| *q).sum();

        match hydra_drive_core::ipc::start_hydra_drive_helper(total, first_root) {
            Ok(_helper) => { /* store helper, e.g. self.helper = Some(helper); */ }
            Err(e) if e.raw_os_error() == Some(1223) => { /* user declined UAC */ }
            Err(_e) => { /* show e in the UI */ }
        }

        self.helper_state = HelperState::Running;
    }

    fn stop_helper(&mut self) {
        // TODO: signal the helper's stop event
        self.helper_state = HelperState::Disabled;
    }

    fn on_admin_toggle(&mut self) {
        if self.admin_support {
            if !self.admin_consent_given {
                self.admin_support = false;
                self.create_after_consent = false;
                self.show_disclosure = true;
            } else if self.drive.is_some() {
                self.start_helper();
            } else {
                self.helper_state = HelperState::NotRunning;
            }
        } else {
            self.stop_helper();
        }
    }

    fn run_link_action(&mut self) {
        let missing = match self.mode {
            Mode::Restore => false,
            Mode::LinkDir => self.source.is_empty() || self.dest.is_empty(),
            Mode::LinkFile => {
                self.source.is_empty() || self.dest.is_empty() || self.app_name.is_empty()
            }
        };
        if missing {
            self.set_status("Please fill in all fields");
            return;
        }

        let result = match self.mode {
            Mode::LinkDir => link_direcories(&self.source, &self.dest),
            Mode::LinkFile => create_syslink(&self.source, &self.dest, &self.app_name),
            Mode::Restore => restore_linked_files(),
        };

        match result {
            Ok(_) => {
                self.set_status("Done");
                self.refresh_names();
                self.refresh_drives();
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }
}

// ── Theme ──────────────────────────────────────────────────────────────

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();

    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.extreme_bg_color = INPUT_BG;
    v.faint_bg_color = Color32::from_rgb(26, 27, 36);

    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_P);
    v.widgets.noninteractive.bg_stroke = Stroke::new(0.5, BORDER);
    v.widgets.noninteractive.rounding = Rounding::same(8.0);

    v.widgets.inactive.bg_fill = Color32::from_rgb(42, 44, 58);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_P);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.rounding = Rounding::same(6.0);

    v.widgets.hovered.bg_fill = Color32::from_rgb(52, 55, 72);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_H);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_D);
    v.widgets.hovered.rounding = Rounding::same(6.0);

    v.widgets.active.bg_fill = ACCENT_D;
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.rounding = Rounding::same(6.0);

    v.widgets.open.bg_fill = Color32::from_rgb(48, 50, 65);
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_H);
    v.widgets.open.rounding = Rounding::same(6.0);

    v.selection.bg_fill = ACCENT.linear_multiply(0.2);
    v.selection.stroke = Stroke::new(1.0, ACCENT);

    v.window_rounding = Rounding::same(12.0);
    v.window_stroke = Stroke::new(1.0, BORDER);

    v.hyperlink_color = ACCENT;
    v.warn_fg_color = AMBER;
    v.error_fg_color = RED;

    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(14.0, 5.0);
    ctx.set_style(style);
}

// ── Reusable UI helpers ────────────────────────────────────────────────

/// Styled card container.
fn card_frame() -> egui::Frame {
    egui::Frame::default()
        .fill(CARD)
        .rounding(Rounding::same(10.0))
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(16.0)
}

/// Section heading with a small accent bar.
fn section_head(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(3.0, 16.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(r, Rounding::same(1.5), ACCENT);
        ui.label(RichText::new(label).color(TEXT_H).size(15.0).strong());
    });
}

/// Animated status dot — pulses when `pulse` is true.
fn pulsing_dot(ui: &mut egui::Ui, color: Color32, label: &str, pulse: bool) {
    ui.horizontal(|ui| {
        let size = Vec2::splat(16.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let c = rect.center();
        let p = ui.painter();

        if pulse {
            let t = ui.ctx().input(|i| i.time);
            let w = ((t * 2.8).sin() as f32) * 0.5 + 0.5; // 0 … 1

            // outer glow ring
            p.circle_filled(c, 5.5 + w * 3.0, color.linear_multiply(0.12));
            // core dot
            p.circle_filled(c, 4.0, color.linear_multiply(0.55 + w * 0.45));

            ui.ctx()
                .request_repaint_after(Duration::from_millis(33));
        } else {
            p.circle_filled(c, 4.0, color);
        }

        ui.label(RichText::new(label).color(TEXT_P).size(13.0));
    });
}

/// Progress bar with a travelling shimmer highlight.
fn shimmer_progress(ui: &mut egui::Ui, fraction: f32, label: String) {
    let desired = Vec2::new(ui.available_width(), 22.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let p = ui.painter();
    let rounding = Rounding::same(11.0);

    // track
    p.rect_filled(rect, rounding, Color32::from_rgb(18, 18, 28));
    p.rect_stroke(rect, rounding, Stroke::new(0.5, BORDER));

    // fill
    if fraction > 0.002 {
        let fw = rect.width() * fraction;
        let fill = Rect::from_min_size(rect.min, Vec2::new(fw, rect.height()));
        p.rect_filled(fill, rounding, ACCENT_D);

        // shimmer band
        let t = ui.ctx().input(|i| i.time) as f32;
        let sx = rect.left() + (t * 0.35).fract() * (fw + 80.0) - 40.0;
        let band = Rect::from_min_size(
            Pos2::new(sx.max(rect.left()), rect.top()),
            Vec2::new(50.0, rect.height()),
        );
        let clipped = fill.intersect(band);
        if clipped.width() > 1.0 && clipped.height() > 1.0 {
            p.rect_filled(clipped, rounding, Color32::from_white_alpha(16));
        }

        ui.ctx()
            .request_repaint_after(Duration::from_millis(33));
    }

    // centred text
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        TEXT_P,
    );
}

/// Accent-coloured primary action button.
fn accent_btn(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(text).color(if enabled { Color32::WHITE } else { TEXT_DIM }),
    )
    .fill(if enabled {
        ACCENT_D
    } else {
        Color32::from_rgb(35, 36, 48)
    });
    ui.add_enabled(enabled, btn)
}

/// Red destructive-action button.
fn danger_btn(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(text).color(if enabled { Color32::WHITE } else { TEXT_DIM }),
    )
    .fill(if enabled {
        DANGER
    } else {
        Color32::from_rgb(35, 36, 48)
    });
    ui.add_enabled(enabled, btn)
}

// ── UI panels ──────────────────────────────────────────────────────────

impl App {
    /// Title row with animated tab underline.
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(RichText::new("Link Manager").color(TEXT_H).size(20.0).strong());
            ui.add_space(24.0);

            let dr = ui.selectable_value(
                &mut self.tab,
                Tab::Drive,
                RichText::new("Virtual Drive").size(14.0),
            );
            let lr = ui.selectable_value(
                &mut self.tab,
                Tab::Links,
                RichText::new("Links").size(14.0),
            );

            // smooth-sliding underline
            let target = if self.tab == Tab::Drive {
                dr.rect
            } else {
                lr.rect
            };
            let al =
                ui.ctx()
                    .animate_value_with_time(Id::new("tab_l"), target.left(), 0.18);
            let ar =
                ui.ctx()
                    .animate_value_with_time(Id::new("tab_r"), target.right(), 0.18);
            let y = target.bottom() + 2.0;
            ui.painter().rect_filled(
                Rect::from_min_max(Pos2::new(al, y), Pos2::new(ar, y + 2.5)),
                Rounding::same(1.5),
                ACCENT,
            );
        });

        // thin separator
        ui.add_space(4.0);
        let r = ui.available_rect_before_wrap();
        ui.painter().rect_filled(
            Rect::from_min_size(r.min, Vec2::new(r.width(), 1.0)),
            Rounding::ZERO,
            BORDER,
        );
        ui.add_space(6.0);
    }

    // ──── Drive tab ────

    fn drive_tab(&mut self, ui: &mut egui::Ui) {
        // ── Status card ──
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_head(ui, "Drive Status");
            ui.add_space(6.0);

            let (dc, dl, dp) = if self.drive.is_some() {
                (GREEN, "Running", true)
            } else {
                (TEXT_DIM, "Not created", false)
            };
            let (hc, hl, hp) = self.helper_state.describe();

            egui::Grid::new("status_grid")
                .num_columns(3)
                .spacing([24.0, 8.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Normal drive").color(TEXT_P).size(13.0));
                    ui.label(RichText::new("V:").color(TEXT_DIM).size(13.0).monospace());
                    pulsing_dot(ui, dc, dl, dp);
                    ui.end_row();

                    ui.label(RichText::new("Admin helper").color(TEXT_P).size(13.0));
                    ui.label(RichText::new("V:").color(TEXT_DIM).size(13.0).monospace());
                    pulsing_dot(ui, hc, hl, hp);
                    ui.end_row();
                });

            if let Some(detail) = self.helper_state.detail() {
                ui.add_space(2.0);
                ui.label(RichText::new(detail).color(RED).size(12.0));
            }

            if self.drive.is_some() {
                ui.add_space(10.0);
                let total: f64 = self.active_roots.iter().map(|(_, q)| *q as f64).sum();
                let total = total.max(1.0);
                let frac = (self.dummy_used_bytes as f64 / total).clamp(0.0, 1.0) as f32;
                shimmer_progress(
                    ui,
                    frac,
                    format!(
                        "{} used of {}",
                        format_bytes(self.dummy_used_bytes),
                        format_bytes(total as u64),
                    ),
                );
            }
        });

        ui.add_space(10.0);

        // ── Drive picker card ──
        let locked = self.drive.is_some();
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                section_head(ui, "Pick drives & split");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!locked, egui::Button::new("Rescan"))
                        .on_hover_text("Re-read the list of drives and their free space")
                        .clicked()
                    {
                        self.refresh_drives();
                    }
                });
            });
            ui.add_space(6.0);

            if self.drives.is_empty() {
                ui.label(
                    RichText::new("No drives found")
                        .color(TEXT_DIM)
                        .italics(),
                );
            }

            for (i, d) in self.drives.iter_mut().enumerate() {
                let usable = d.usable();
                egui::Frame::default()
                    .fill(Color32::from_rgb(26, 27, 36))
                    .rounding(Rounding::same(8.0))
                    .stroke(Stroke::new(
                        1.0,
                        if d.enabled { ACCENT_D } else { BORDER },
                    ))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        ui.horizontal(|ui| {
                            let label = if d.info.name.is_empty() {
                                format!("{}", d.info.mount.display())
                            } else {
                                format!("{}  ({})", d.info.mount.display(), d.info.name)
                            };
                            let changed = ui
                                .add_enabled(
                                    !locked && usable,
                                    egui::Checkbox::new(
                                        &mut d.enabled,
                                        RichText::new(label).color(TEXT_H).strong(),
                                    ),
                                )
                                .changed();
                            // first time a drive is ticked, offer half of what it can give
                            if changed && d.enabled && d.bytes < GIB {
                                d.bytes = ((d.max_alloc() / 2.0) / GIB).floor().max(1.0) * GIB;
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} free of {}",
                                            format_bytes(d.info.free),
                                            format_bytes(d.info.total),
                                        ))
                                        .color(TEXT_DIM)
                                        .size(12.0),
                                    );
                                },
                            );
                        });

                        if !usable {
                            ui.label(
                                RichText::new("Not enough free space (needs more than 2 GB)")
                                    .color(AMBER)
                                    .size(12.0),
                            );
                        } else if d.enabled {
                            ui.add_space(2.0);
                            let max = d.max_alloc();
                            ui.add_enabled(
                                !locked,
                                Slider::new(&mut d.bytes, 0.0..=max)
                                    .step_by(GIB)
                                    .text("Use from this drive")
                                    .custom_formatter(|v, _| format_bytes(v as u64)),
                            );

                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("Folder: {}", d.folder.display()))
                                        .color(TEXT_DIM)
                                        .size(12.0)
                                        .monospace(),
                                );
                                if ui
                                    .add_enabled(!locked, egui::Button::new("Change\u{2026}"))
                                    .clicked()
                                {
                                    if let Some(p) = rfd::FileDialog::new()
                                        .set_directory(&d.info.mount)
                                        .pick_folder()
                                    {
                                        // the folder must stay on the drive it was picked for
                                        if p.starts_with(&d.info.mount) {
                                            d.folder = p;
                                        }
                                    }
                                }
                            });
                        }
                    });
                ui.add_space(2.0);
                let _ = i;
            }

            ui.add_space(6.0);
            let picked = self.drives.iter().filter(|d| d.enabled).count();
            ui.label(
                RichText::new(format!(
                    "Virtual drive size: {}  ({} drive{} selected)",
                    format_bytes(self.total_selected() as u64),
                    picked,
                    if picked == 1 { "" } else { "s" },
                ))
                .color(TEXT_H)
                .size(13.5)
                .strong(),
            );

            ui.add_space(6.0);
            let before = self.admin_support;
            ui.checkbox(
                &mut self.admin_support,
                RichText::new("Support installers running as administrator").color(TEXT_P),
            )
            .on_hover_text(
                "Starts a small helper that creates a second copy of the drive \
                 so installers running as administrator can see it.",
            );
            if self.admin_support != before {
                self.on_admin_toggle();
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if accent_btn(ui, "Create Virtual Drive", self.drive.is_none()).clicked() {
                    self.request_create();
                }
                ui.add_space(4.0);
                if danger_btn(ui, "Delete Virtual Drive", self.drive.is_some()).clicked() {
                    self.delete_drive();
                }
            });
        });

        // ── Debug controls (dev builds only) ──
        #[cfg(debug_assertions)]
        {
            ui.add_space(10.0);
            ui.collapsing(
                RichText::new("Dummy data (dev only)")
                    .color(TEXT_DIM)
                    .size(12.0),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Helper state").color(TEXT_P));
                        egui::ComboBox::from_id_salt("dummy_helper_state")
                            .selected_text(self.helper_state.describe().1)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.helper_state,
                                    HelperState::Disabled,
                                    "Off",
                                );
                                ui.selectable_value(
                                    &mut self.helper_state,
                                    HelperState::NotRunning,
                                    "Not running",
                                );
                                ui.selectable_value(
                                    &mut self.helper_state,
                                    HelperState::Starting,
                                    "Starting",
                                );
                                ui.selectable_value(
                                    &mut self.helper_state,
                                    HelperState::Running,
                                    "Running",
                                );
                                ui.selectable_value(
                                    &mut self.helper_state,
                                    HelperState::Failed("UAC prompt declined".into()),
                                    "Failed",
                                );
                            });
                    });
                    let mut used_gib = self.dummy_used_bytes as f64 / GIB;
                    if ui
                        .add(Slider::new(&mut used_gib, 0.0..=20.0).text("Used (GB)"))
                        .changed()
                    {
                        self.dummy_used_bytes = (used_gib * GIB) as u64;
                    }
                },
            );
        }
    }

    // ──── Links tab ────

    fn links_tab(&mut self, ui: &mut egui::Ui) {
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_head(ui, "Link Manager");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut self.mode,
                    Mode::LinkDir,
                    RichText::new("Link directory").color(TEXT_P),
                );
                ui.radio_value(
                    &mut self.mode,
                    Mode::LinkFile,
                    RichText::new("Link file").color(TEXT_P),
                );
                ui.radio_value(
                    &mut self.mode,
                    Mode::Restore,
                    RichText::new("Restore all").color(TEXT_P),
                );
            });

            ui.add_space(8.0);

            if self.mode != Mode::Restore {
                ui.label(RichText::new("Current path").color(TEXT_DIM).size(12.0));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.source)
                            .desired_width(ui.available_width() - 160.0),
                    );
                    if ui.button("File\u{2026}").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.source = path.display().to_string();
                        }
                    }
                    if ui.button("Folder\u{2026}").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.source = path.display().to_string();
                        }
                    }
                });

                ui.add_space(6.0);
                ui.label(RichText::new("Destination").color(TEXT_DIM).size(12.0));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.dest)
                            .desired_width(ui.available_width() - 90.0),
                    );
                    if ui.button("Browse\u{2026}").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.dest = path.display().to_string();
                        }
                    }
                });
            }

            if self.mode == Mode::LinkFile {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Application name")
                        .color(TEXT_DIM)
                        .size(12.0),
                );
                ui.add_space(2.0);
                ui.text_edit_singleline(&mut self.app_name);
            }

            ui.add_space(10.0);
            if accent_btn(ui, "Start", true).clicked() {
                self.run_link_action();
            }
        });

        ui.add_space(10.0);

        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_head(ui, "Linked Applications");
            ui.add_space(6.0);

            if self.app_names.is_empty() {
                ui.label(
                    RichText::new("No linked applications found")
                        .color(TEXT_DIM)
                        .italics(),
                );
            } else {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for (i, name) in self.app_names.iter().enumerate() {
                            let bg = if i % 2 == 1 {
                                Color32::from_rgb(26, 27, 36)
                            } else {
                                Color32::TRANSPARENT
                            };
                            egui::Frame::default()
                                .fill(bg)
                                .rounding(4.0)
                                .inner_margin(Vec2::new(8.0, 3.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(name).color(TEXT_P).size(13.0),
                                    );
                                });
                        }
                    });
            }
        });
    }

    // ──── Disclosure window ────

    fn disclosure_window(&mut self, ctx: &egui::Context) {
        if !self.show_disclosure {
            return;
        }
        let mut enable = false;
        let mut decline = false;

        egui::Window::new(
            RichText::new("Enable administrator installer support?").strong(),
        )
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(480.0);

            ui.label(RichText::new(
                "Link Manager creates a virtual drive (V:) backed by normal folders on your disks.",
            ).color(TEXT_P));

            ui.add_space(8.0);

            for (heading, body) in [
                (
                    "Why a second program is needed",
                    "Windows keeps drives separate for normal programs and for programs running \
                     as administrator. Many installers run as administrator, so without the helper \
                     they would not see V: and you could not pick it as an install location. \
                     The helper creates a second copy of the same drive for administrator programs. \
                     Both drives show the same files.",
                ),
                (
                    "What you will see",
                    "Windows will ask for administrator permission (UAC) when the helper starts. \
                     The helper runs in the background and uses very little memory and almost no \
                     CPU while idle.",
                ),
                (
                    "Why it stays open",
                    "A virtual drive only exists while the program that created it is running. \
                     If the helper closes, the administrator copy of the drive disappears, and so \
                     does the main drive if Link Manager closes. Your files are not deleted, \
                     because they stay in the Virtual_Drive_Pool folders. The drive is simply gone \
                     until it is created again.",
                ),
                (
                    "When it closes",
                    "The helper closes automatically when you close Link Manager or turn this \
                     option off.",
                ),
                (
                    "Your choice",
                    "You can decline. The normal drive is still created, but installers running \
                     as administrator will not be able to see it.",
                ),
            ] {
                ui.label(RichText::new(heading).color(ACCENT).strong());
                ui.add_space(2.0);
                ui.label(RichText::new(body).color(TEXT_P).size(13.0));
                ui.add_space(8.0);
            }

            // separator
            let r = ui.available_rect_before_wrap();
            ui.painter().rect_filled(
                Rect::from_min_size(r.min, Vec2::new(r.width(), 1.0)),
                Rounding::ZERO,
                BORDER,
            );
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                if accent_btn(ui, "Enable", true).clicked() {
                    enable = true;
                }
                ui.add_space(8.0);
                if ui
                    .button(RichText::new("Not now").color(TEXT_P))
                    .clicked()
                {
                    decline = true;
                }
            });
        });

        if enable {
            self.admin_consent_given = true;
            self.admin_support = true;
            self.show_disclosure = false;
            if self.create_after_consent {
                self.create_after_consent = false;
                self.do_create();
            } else if self.drive.is_some() {
                self.start_helper();
            } else {
                self.helper_state = HelperState::NotRunning;
            }
        } else if decline {
            self.admin_support = false;
            self.helper_state = HelperState::Disabled;
            self.show_disclosure = false;
            if self.create_after_consent {
                self.create_after_consent = false;
                self.do_create();
            }
        }
    }
}

// ── eframe::App ────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // apply the custom theme once
        if !self.theme_applied {
            apply_theme(ctx);
            self.theme_applied = true;
        }

        // ── Status bar (bottom) ──
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(20, 20, 28))
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin {
                        left: 12.0,
                        right: 12.0,
                        top: 6.0,
                        bottom: 6.0,
                    }),
            )
            .show(ctx, |ui| {
                let dot_color = if self.status.starts_with("Error") {
                    RED
                } else if self.status.contains("mounted")
                    || self.status == "Done"
                    || self.status.contains("removed")
                {
                    GREEN
                } else {
                    TEXT_DIM
                };

                // gently fade the message after a few seconds
                let age = self.status_changed.elapsed().as_secs_f32();
                let alpha = if age < 4.0 {
                    1.0
                } else {
                    (1.0 - (age - 4.0) / 3.0).max(0.45)
                };

                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(r.center(), 3.5, dot_color.linear_multiply(alpha));
                    ui.label(
                        RichText::new(&self.status)
                            .color(TEXT_P.linear_multiply(alpha))
                            .size(12.5),
                    );
                });

                // keep repainting while the fade is in progress
                if alpha < 1.0 && alpha > 0.46 {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            });

        // ── Central panel ──
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(PANEL)
                    .inner_margin(egui::Margin {
                        left: 16.0,
                        right: 16.0,
                        top: 12.0,
                        bottom: 8.0,
                    }),
            )
            .show(ctx, |ui| {
                self.header(ui);
                ui.add_space(4.0);

                egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                    Tab::Drive => self.drive_tab(ui),
                    Tab::Links => self.links_tab(ui),
                });
            });

        self.disclosure_window(ctx);
    }
}

// ── Entry point ────────────────────────────────────────────────────────

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([660.0, 760.0])
            .with_min_inner_size([560.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Link Manager",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}
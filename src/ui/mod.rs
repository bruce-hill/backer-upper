pub mod style;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::{Color32, RichText, Stroke};

use crate::backup::{rsync_command_string, run_backup, BackupProgress, SharedProgress};
use crate::config::{Config, SyncJob, SyncMode};
use crate::drives::{self, Drive};
use style::*;

#[derive(Debug, Clone, PartialEq)]
enum Screen {
    DriveSelect,
    PasswordPrompt,
    ConfigEditor,
    Preview,
    Backup,
    JobEdit(usize),
}

pub struct App {
    screen: Screen,
    drives: Vec<Drive>,
    selected_drive_idx: Option<usize>,
    password: String,
    password_error: Option<String>,
    mount_point: Option<PathBuf>,
    mounted_device: Option<String>,  // original /dev/… used to open the drive
    mapper_name: Option<String>,     // cleartext D-Bus obj path if LUKS, else None
    config: Option<Config>,
    config_dirty: bool,
    progress: SharedProgress,
    backup_running: bool,
    backup_finished_msg: Option<String>,
    // Job edit temps
    edit_source: String,
    edit_name: String,
    edit_dest: String,
    edit_excludes: String,
    edit_mode: SyncMode,
    edit_enabled: bool,
    status_msg: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        App {
            screen: Screen::DriveSelect,
            drives: Vec::new(),
            selected_drive_idx: None,
            password: String::new(),
            password_error: None,
            mount_point: None,
            mounted_device: None,
            mapper_name: None,
            config: None,
            config_dirty: false,
            progress: Arc::new(Mutex::new(BackupProgress {
                current_job: 0,
                total_jobs: 0,
                job_name: String::new(),
                files_transferred: 0,
                files_total: 0,
                bytes_transferred: 0,
                bytes_total: 0,
                current_file: String::new(),
                elapsed_secs: 0.0,
                estimated_total_secs: None,
                finished: false,
                cancelled: false,
                error: None,
                log_lines: Vec::new(),
                child_pid: None,
                paused: false,
            })),
            backup_running: false,
            backup_finished_msg: None,
            edit_source: String::new(),
            edit_name: String::new(),
            edit_dest: String::new(),
            edit_excludes: String::new(),
            edit_mode: SyncMode::Backup,
            edit_enabled: true,
            status_msg: None,
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_xp_style(&cc.egui_ctx);
        let mut app = App::default();
        app.refresh_drives();
        app
    }

    fn refresh_drives(&mut self) {
        match drives::list_removable_drives() {
            Ok(d) => {
                self.drives = d;
                self.status_msg = None;
            }
            Err(e) => {
                self.status_msg = Some(format!("Error listing drives: {e}"));
            }
        }
    }

    fn selected_drive(&self) -> Option<&Drive> {
        self.selected_drive_idx.and_then(|i| self.drives.get(i))
    }

    fn try_mount_selected(&mut self) {
        let Some(drive) = self.selected_drive().cloned() else {
            return;
        };

        if drive.is_mounted() {
            let mp = PathBuf::from(drive.mountpoint.as_deref().unwrap_or("/"));
            self.mount_point = Some(mp.clone());
            self.load_config_from(&mp);
            self.screen = Screen::ConfigEditor;
            return;
        }

        if drive.is_encrypted {
            self.screen = Screen::PasswordPrompt;
        } else {
            match drives::mount_device(&drive.device) {
                Ok(mp) => {
                    self.mounted_device = Some(drive.device.clone());
                    self.mount_point = Some(mp.clone());
                    self.load_config_from(&mp);
                    self.screen = Screen::ConfigEditor;
                }
                Err(e) => {
                    self.status_msg = Some(format!("Mount failed: {e}"));
                }
            }
        }
    }

    fn try_unlock_and_mount(&mut self) {
        let Some(drive) = self.selected_drive().cloned() else {
            return;
        };

        match drives::unlock_and_mount(&drive.device, &self.password) {
            Ok((dm_device, mp)) => {
                self.password.clear();
                self.password_error = None;
                self.mounted_device = Some(drive.device.clone());
                self.mapper_name = Some(dm_device);
                self.mount_point = Some(mp.clone());
                self.load_config_from(&mp);
                self.screen = Screen::ConfigEditor;
            }
            Err(e) => {
                self.password_error = Some(format!("Unlock failed: {e}"));
            }
        }
    }

    fn load_config_from(&mut self, mp: &PathBuf) {
        match Config::load(mp) {
            Ok(cfg) => {
                self.config = Some(cfg);
                self.config_dirty = false;
            }
            Err(e) => {
                self.status_msg = Some(format!("Config error: {e}"));
                self.config = Some(Config::default());
            }
        }
    }

    fn save_config(&mut self) {
        if let (Some(cfg), Some(mp)) = (&self.config, &self.mount_point) {
            if let Err(e) = cfg.save(mp) {
                self.status_msg = Some(format!("Save failed: {e}"));
            } else {
                self.config_dirty = false;
                self.status_msg = Some("Configuration saved.".to_owned());
            }
        }
    }

    fn eject(&mut self) {
        let result = match (&self.mapper_name, &self.mounted_device) {
            // LUKS: unmount cleartext object, then lock the original device
            (Some(cleartext_obj), Some(luks_dev)) => {
                let r1 = drives::unmount_filesystem(cleartext_obj);
                let r2 = drives::lock_luks(luks_dev);
                r1.and(r2)
            }
            // Plain: unmount by original device path
            (None, Some(dev)) => drives::unmount_device(dev),
            _ => Ok(()),
        };

        match result {
            Ok(()) => {
                self.mount_point = None;
                self.mounted_device = None;
                self.mapper_name = None;
                self.config = None;
                self.config_dirty = false;
                self.backup_finished_msg = None;
                self.screen = Screen::DriveSelect;
                self.refresh_drives();
            }
            Err(e) => {
                self.status_msg = Some(format!("Eject failed: {e}"));
            }
        }
    }

    fn start_backup(&mut self) {
        let (Some(cfg), Some(mp)) = (self.config.clone(), self.mount_point.clone()) else {
            return;
        };

        // Reset progress
        {
            let mut p = self.progress.lock().unwrap();
            *p = BackupProgress {
                current_job: 0,
                total_jobs: 0,
                job_name: String::new(),
                files_transferred: 0,
                files_total: 0,
                bytes_transferred: 0,
                bytes_total: 0,
                current_file: String::new(),
                elapsed_secs: 0.0,
                estimated_total_secs: None,
                finished: false,
                cancelled: false,
                error: None,
                log_lines: Vec::new(),
                child_pid: None,
                paused: false,
            };
        }

        self.backup_running = true;
        self.backup_finished_msg = None;
        self.screen = Screen::Backup;

        run_backup(&cfg, &mp, Arc::clone(&self.progress));
    }

    fn start_editing_job(&mut self, idx: usize) {
        if let Some(cfg) = &self.config {
            if let Some(job) = cfg.jobs.get(idx) {
                self.edit_name = job.name.clone();
                self.edit_source = job.source.display().to_string();
                self.edit_dest = job.destination.display().to_string();
                self.edit_excludes = job.excludes.join("\n");
                self.edit_mode = job.mode.clone();
                self.edit_enabled = job.enabled;
                self.screen = Screen::JobEdit(idx);
            }
        }
    }

    fn commit_job_edit(&mut self, idx: usize) {
        let job = SyncJob {
            name: self.edit_name.clone(),
            source: PathBuf::from(&self.edit_source),
            destination: PathBuf::from(&self.edit_dest),
            excludes: self
                .edit_excludes
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            mode: self.edit_mode.clone(),
            enabled: self.edit_enabled,
        };

        if let Some(cfg) = &mut self.config {
            if idx < cfg.jobs.len() {
                cfg.jobs[idx] = job;
            } else {
                cfg.jobs.push(job);
            }
            self.config_dirty = true;
        }
        self.screen = Screen::ConfigEditor;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll backup progress
        if self.backup_running {
            let (finished, cancelled, err_msg, elapsed_secs) = {
                let p = self.progress.lock().unwrap();
                (p.finished || p.error.is_some(), p.cancelled, p.error.clone(), p.elapsed_secs)
            };
            if finished {
                self.backup_running = false;
                if cancelled {
                    self.backup_finished_msg = Some("Backup cancelled.".to_owned());
                } else if let Some(err) = err_msg {
                    self.backup_finished_msg = Some(format!("Backup failed: {err}"));
                } else {
                    let elapsed = format_duration(elapsed_secs as u64);
                    self.backup_finished_msg =
                        Some(format!("Backup complete in {elapsed}!"));

                    if let Some(cfg) = &mut self.config {
                        cfg.last_backup = Some(chrono::Local::now());
                    }
                    if !self.config_dirty {
                        self.save_config();
                    }
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        draw_title_bar(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(XP_BG).inner_margin(egui::Margin::same(12)))
            .show(ctx, |ui| {
                match self.screen.clone() {
                    Screen::DriveSelect => self.ui_drive_select(ui),
                    Screen::PasswordPrompt => self.ui_password(ui),
                    Screen::ConfigEditor => self.ui_config(ui),
                    Screen::Preview => self.ui_preview(ui),
                    Screen::Backup => self.ui_backup(ui),
                    Screen::JobEdit(idx) => self.ui_job_edit(ui, idx),
                }

                if let Some(msg) = &self.status_msg.clone() {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(120, 80, 0), msg);
                }
            });
    }
}

fn draw_title_bar(ctx: &egui::Context) {
    egui::TopBottomPanel::top("title_bar")
        .exact_height(30.0)
        .frame(egui::Frame::new().fill(XP_TITLE_BG))
        .show(ctx, |ui| {
            // Smooth gradient over the full panel rect
            paint_v_gradient(ui.painter(), ui.max_rect(), XP_TITLE_BG_TOP, XP_TITLE_BG);

            ui.horizontal_centered(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Backer-Upper")
                        .color(XP_TITLE_TEXT)
                        .size(14.0)
                        .strong(),
                );
                let available = ui.available_width();
                ui.add_space(available - 32.0);
                if xp_close_button_ui(ui).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ui.add_space(4.0);
            });
        });
}

// ── Screen renderers ────────────────────────────────────────────────────────

impl App {
    fn ui_drive_select(&mut self, ui: &mut egui::Ui) {
        ui.heading("Select External Drive");
        ui.separator();
        ui.add_space(4.0);

        if self.drives.is_empty() {
            ui.label("No removable drives detected.");
        } else {
            egui::Frame::new()
                .fill(XP_GROUP_BG)
                .stroke(egui::Stroke::new(1.0, XP_BORDER))
                .corner_radius(3.0)
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (i, drive) in self.drives.iter().enumerate() {
                                let selected = self.selected_drive_idx == Some(i);
                                if drive_row_ui(ui, drive, selected).clicked() {
                                    self.selected_drive_idx = Some(i);
                                }
                            }
                        });
                });
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if xp_button_ui(ui, "Refresh", false).clicked() {
                self.refresh_drives();
            }
            ui.add_space(8.0);
            let can_open = self.selected_drive_idx.is_some();
            ui.add_enabled_ui(can_open, |ui| {
                if xp_button_ui(ui, "Open Drive", false).clicked() {
                    self.try_mount_selected();
                }
            });
        });
    }

    fn ui_password(&mut self, ui: &mut egui::Ui) {
        let drive_name = self
            .selected_drive()
            .map(|d| d.display_name())
            .unwrap_or_default();

        ui.heading(format!("Unlock: {drive_name}"));
        ui.separator();
        ui.add_space(8.0);

        ui.label("This drive is LUKS-encrypted. Enter the passphrase:");
        ui.add_space(6.0);

        let response = ui.add(
            egui::TextEdit::singleline(&mut self.password)
                .password(true)
                .hint_text("Passphrase")
                .desired_width(300.0),
        );

        if let Some(err) = &self.password_error {
            ui.colored_label(Color32::RED, err);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if xp_button_ui(ui, "Cancel", false).clicked() {
                self.screen = Screen::DriveSelect;
                self.password.clear();
                self.password_error = None;
            }
            ui.add_space(8.0);
            if xp_button_ui(ui, "Unlock", false).clicked()
                || (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                self.try_unlock_and_mount();
            }
        });
    }

    fn ui_config(&mut self, ui: &mut egui::Ui) {
        let mp = self
            .mount_point
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let mut action: Option<ConfigAction> = None;

        ui.horizontal(|ui| {
            ui.heading("Backup Configuration");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if xp_button_ui(ui, "Eject", false).clicked() {
                    action = Some(ConfigAction::Eject);
                }
            });
        });
        ui.label(
            RichText::new(format!("Drive: {mp}"))
                .small()
                .color(Color32::DARK_GRAY),
        );
        ui.separator();
        ui.add_space(4.0);

        // Jobs table
        let jobs_len = self
            .config
            .as_ref()
            .map(|c| c.jobs.len())
            .unwrap_or(0);

        egui::Frame::new()
            .fill(XP_GROUP_BG)
            .stroke(Stroke::new(1.0, XP_BORDER))
            .corner_radius(2.0)
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                egui::Grid::new("jobs_grid")
                    .num_columns(5)
                    .spacing([8.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("Source");
                        ui.strong("Mode");
                        ui.strong("On");
                        ui.strong("");
                        ui.end_row();

                        for i in 0..jobs_len {
                            if let Some(cfg) = &self.config {
                                if let Some(job) = cfg.jobs.get(i) {
                                    ui.label(&job.name);
                                    ui.label(
                                        RichText::new(job.source.display().to_string())
                                            .monospace()
                                            .size(10.0),
                                    );
                                    ui.label(job.mode.label());

                                    let mut enabled = job.enabled;
                                    if ui.checkbox(&mut enabled, "").changed() {
                                        action = Some(ConfigAction::ToggleJob(i, enabled));
                                    }

                                    if xp_button_ui(ui, "Edit", false).clicked() {
                                        action = Some(ConfigAction::EditJob(i));
                                    }
                                    ui.end_row();
                                }
                            }
                        }
                    });
            });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if xp_button_ui(ui, "+ Add Job", false).clicked() {
                action = Some(ConfigAction::AddJob);
            }
            if self.config_dirty {
                ui.add_space(8.0);
                if xp_button_ui(ui, "Save Config", false).clicked() {
                    action = Some(ConfigAction::Save);
                }
                ui.colored_label(Color32::from_rgb(180, 90, 0), "Unsaved changes");
            }
        });

        // Last backup info
        if let Some(cfg) = &self.config {
            if let Some(ts) = cfg.last_backup {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Last backup: {}",
                        ts.format("%Y-%m-%d %H:%M:%S")
                    ))
                    .small()
                    .color(Color32::DARK_GRAY),
                );
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // The big backup button → goes to preview first
        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
            if xp_button_ui(ui, "Backup Now", true).clicked() {
                if self.config_dirty {
                    self.save_config();
                }
                self.screen = Screen::Preview;
            }
        });

        // Process actions after borrow ends
        match action {
            Some(ConfigAction::EditJob(i)) => self.start_editing_job(i),
            Some(ConfigAction::ToggleJob(i, v)) => {
                if let Some(cfg) = &mut self.config {
                    if let Some(j) = cfg.jobs.get_mut(i) {
                        j.enabled = v;
                        self.config_dirty = true;
                    }
                }
            }
            Some(ConfigAction::AddJob) => {
                if let Some(cfg) = &mut self.config {
                    let idx = cfg.jobs.len();
                    cfg.jobs.push(SyncJob::new(
                        format!("Job {}", idx + 1),
                        std::env::var("HOME").unwrap_or_default(),
                    ));
                    self.config_dirty = true;
                    self.start_editing_job(idx);
                }
            }
            Some(ConfigAction::Save) => self.save_config(),
            Some(ConfigAction::Eject) => self.eject(),
            None => {}
        }
    }

    fn ui_job_edit(&mut self, ui: &mut egui::Ui, idx: usize) {
        ui.heading(if idx < self.config.as_ref().map(|c| c.jobs.len()).unwrap_or(0) {
            "Edit Sync Job"
        } else {
            "New Sync Job"
        });
        ui.separator();
        ui.add_space(6.0);

        egui::Grid::new("job_edit_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut self.edit_name);
                ui.end_row();

                ui.label("Source folder:");
                ui.text_edit_singleline(&mut self.edit_source);
                ui.end_row();

                ui.label("Dest (on drive):");
                ui.text_edit_singleline(&mut self.edit_dest);
                ui.end_row();

                ui.label("Mode:");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.edit_mode, SyncMode::Backup, SyncMode::Backup.label());
                    ui.label(
                        RichText::new(SyncMode::Backup.description())
                            .small()
                            .color(Color32::DARK_GRAY),
                    );
                });
                ui.end_row();

                ui.label("");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.edit_mode, SyncMode::Media, SyncMode::Media.label());
                    ui.label(
                        RichText::new(SyncMode::Media.description())
                            .small()
                            .color(Color32::DARK_GRAY),
                    );
                });
                ui.end_row();

                ui.label("Enabled:");
                ui.checkbox(&mut self.edit_enabled, "");
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.label("Excludes (one per line, rsync patterns):");
        ui.add(
            egui::TextEdit::multiline(&mut self.edit_excludes)
                .desired_rows(4)
                .desired_width(400.0)
                .font(egui::TextStyle::Monospace),
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if xp_button_ui(ui, "Cancel", false).clicked() {
                self.screen = Screen::ConfigEditor;
            }
            ui.add_space(8.0);
            if xp_button_ui(ui, "Save Job", false).clicked() {
                self.commit_job_edit(idx);
            }
        });
    }

    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        ui.heading("Review Backup Commands");
        ui.separator();
        ui.add_space(6.0);

        // Build the command strings up front so borrows are released before the buttons
        let commands: Vec<(String, String)> = match (&self.config, &self.mount_point) {
            (Some(cfg), Some(mp)) => cfg
                .jobs
                .iter()
                .filter(|j| j.enabled)
                .map(|j| (j.name.clone(), rsync_command_string(j, mp)))
                .collect(),
            _ => {
                ui.label("No configuration or drive available.");
                return;
            }
        };

        if commands.is_empty() {
            ui.label("No enabled jobs.");
        } else {
            ui.label(
                RichText::new("These rsync commands will run in order:")
                    .color(Color32::DARK_GRAY),
            );
            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for (name, cmd) in &commands {
                        ui.add_space(4.0);
                        ui.label(RichText::new(name).strong());
                        egui::Frame::new()
                            .fill(Color32::from_rgb(30, 30, 30))
                            .corner_radius(3.0)
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(cmd)
                                            .monospace()
                                            .size(11.5)
                                            .color(Color32::from_rgb(200, 240, 200)),
                                    )
                                    .wrap(),
                                );
                            });
                    }
                });
        }

        let any_jobs = !commands.is_empty();
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        let mut run = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            cancel = xp_button_ui(ui, "Cancel", false).clicked();
            ui.add_space(8.0);
            ui.add_enabled_ui(any_jobs, |ui| {
                run = xp_button_ui(ui, "Run Backup", true).clicked();
            });
        });
        if cancel { self.screen = Screen::ConfigEditor; }
        if run { self.start_backup(); }
    }

    fn ui_backup(&mut self, ui: &mut egui::Ui) {
        ui.heading("Backing Up…");
        ui.separator();
        ui.add_space(8.0);

        let (fraction, job_name, current_file, eta, finished, cancelled, error, log_lines, elapsed, paused, child_pid) = {
            let p = self.progress.lock().unwrap();
            (
                p.overall_fraction(),
                p.job_name.clone(),
                p.current_file.clone(),
                p.eta_string(),
                p.finished,
                p.cancelled,
                p.error.clone(),
                p.log_lines.clone(),
                p.elapsed_secs,
                p.paused,
                p.child_pid,
            )
        };
        let running = !finished && error.is_none();

        if let Some(err) = &error {
            ui.colored_label(Color32::RED, format!("Error: {err}"));
        } else if finished {
            let color = if cancelled {
                Color32::from_rgb(160, 100, 0)
            } else {
                Color32::from_rgb(0, 128, 0)
            };
            if let Some(msg) = &self.backup_finished_msg {
                ui.colored_label(color, msg);
            }
        } else if paused {
            ui.colored_label(Color32::from_rgb(160, 100, 0), "Paused");
            ui.small(format!("Job: {job_name}"));
        } else {
            ui.label(format!("Job: {job_name}"));
            ui.small(format!("File: {current_file}"));
            ui.small(format!("ETA: {eta}"));
        }

        ui.add_space(8.0);

        // Progress bar
        let bar_rect = {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 28.0),
                egui::Sense::hover(),
            );

            let painter = ui.painter();
            let bg_color = Color32::from_rgb(220, 220, 220);
            let fill_color = if error.is_some() {
                Color32::from_rgb(200, 50, 50)
            } else if finished {
                Color32::from_rgb(0, 160, 0)
            } else {
                Color32::from_rgb(56, 142, 60)
            };

            painter.rect_filled(rect, egui::CornerRadius::same(3), bg_color);

            let fill_width = rect.width() * fraction;
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_width, rect.height()));

            // Gradient green
            let fill_top = Color32::from_rgb(
                fill_color.r().saturating_add(40),
                fill_color.g().saturating_add(40),
                fill_color.b().saturating_add(20),
            );
            let half_y = fill_rect.center().y;
            painter.rect_filled(
                egui::Rect::from_min_max(fill_rect.min, egui::pos2(fill_rect.max.x, half_y)),
                egui::CornerRadius { nw: 3, ne: if fill_width >= rect.width() { 3 } else { 0 }, sw: 0, se: 0 },
                fill_top,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(fill_rect.min.x, half_y), fill_rect.max),
                egui::CornerRadius { nw: 0, ne: 0, sw: 3, se: if fill_width >= rect.width() { 3 } else { 0 } },
                fill_color,
            );

            painter.rect_stroke(rect, egui::CornerRadius::same(3), egui::Stroke::new(1.0, XP_BORDER), egui::StrokeKind::Outside);

            let pct_text = format!("{:.0}%", fraction * 100.0);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &pct_text,
                egui::FontId::new(13.0, egui::FontFamily::Proportional),
                Color32::WHITE,
            );

            rect
        };

        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Elapsed: {}", format_duration(elapsed as u64)))
                .small()
                .color(Color32::DARK_GRAY),
        );

        ui.add_space(8.0);

        // Log output
        ui.collapsing("Output Log", |ui| {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &log_lines {
                        ui.monospace(line);
                    }
                });
        });

        ui.add_space(8.0);

        let mut do_pause_toggle = false;
        let mut do_cancel = false;
        ui.horizontal(|ui| {
            if !running {
                if xp_button_ui(ui, "Back to Config", false).clicked() {
                    self.screen = Screen::ConfigEditor;
                }
                ui.add_space(8.0);
                if xp_button_ui(ui, "Eject Drive", false).clicked() {
                    self.eject();
                }
            } else {
                let pause_label = if paused { "Resume" } else { "Pause" };
                if xp_button_ui(ui, pause_label, false).clicked() {
                    do_pause_toggle = true;
                }
                ui.add_space(8.0);
                if xp_button_ui(ui, "Cancel", false).clicked() {
                    do_cancel = true;
                }
            }
        });

        if do_pause_toggle {
            if let Some(pid) = child_pid {
                let sig = if paused { "CONT" } else { "STOP" };
                let _ = std::process::Command::new("kill")
                    .args([&format!("-{sig}"), &pid.to_string()])
                    .status();
                self.progress.lock().unwrap().paused = !paused;
            }
        }
        if do_cancel {
            let mut p = self.progress.lock().unwrap();
            p.cancelled = true;
            p.finished = true;
            if let Some(pid) = p.child_pid {
                drop(p); // release lock before spawning subprocess
                let _ = std::process::Command::new("kill")
                    .args([&pid.to_string()])
                    .status();
            }
        }

        let _ = bar_rect;
    }
}

enum ConfigAction {
    EditJob(usize),
    ToggleJob(usize, bool),
    AddJob,
    Save,
    Eject,
}

fn drive_row_ui(ui: &mut egui::Ui, drive: &Drive, selected: bool) -> egui::Response {
    let row_height = 52.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_height),
        egui::Sense::click(),
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();

        // Row background
        let bg = if selected {
            XP_ACCENT
        } else if response.hovered() {
            Color32::from_rgb(215, 228, 252)
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            painter.rect_filled(rect, egui::CornerRadius::same(3), bg);
        }

        let text_color   = if selected { Color32::WHITE } else { XP_TEXT };
        let detail_color = if selected {
            Color32::from_rgba_unmultiplied(210, 225, 255, 200)
        } else {
            Color32::from_rgb(80, 95, 130)
        };

        let px = 10.0;   // horizontal pad
        let py = 8.0;    // top pad

        // ── Row 1: name (left) + size (right) ───────────────────────────────
        let name = drive.display_name();
        let name_galley = painter.layout_no_wrap(
            name,
            egui::FontId::new(13.5, egui::FontFamily::Proportional),
            text_color,
        );
        painter.galley(rect.min + egui::vec2(px, py), name_galley, text_color);

        if let Some(size) = &drive.size {
            let size_galley = painter.layout_no_wrap(
                size.clone(),
                egui::FontId::new(12.0, egui::FontFamily::Proportional),
                detail_color,
            );
            let x = rect.max.x - px - size_galley.size().x;
            painter.galley(egui::pos2(x, rect.min.y + py + 1.0), size_galley, detail_color);
        }

        // ── Row 2: device path + badges ─────────────────────────────────────
        let row2_y = rect.min.y + py + 22.0;

        // Only show the device path if it's not the same as the display name
        let show_device = drive.label.as_deref().unwrap_or("") != drive.device
            && drive.display_name() != drive.device;
        let dev_w = if show_device || drive.label.is_none() {
            let dev_galley = painter.layout_no_wrap(
                drive.device.clone(),
                egui::FontId::new(11.0, egui::FontFamily::Monospace),
                detail_color,
            );
            let w = dev_galley.size().x;
            painter.galley(egui::pos2(rect.min.x + px, row2_y), dev_galley, detail_color);
            w
        } else {
            0.0
        };

        // Badges: transport, filesystem, LUKS, mounted
        let mut badges: Vec<(String, Color32)> = Vec::new();
        if let Some(tran) = &drive.tran {
            badges.push((tran.to_uppercase(), Color32::from_rgb(49, 106, 197)));
        }
        if let Some(fs) = &drive.fstype {
            // Skip showing the raw fstype for LUKS — we have the LUKS badge for that
            if fs != "crypto_LUKS" {
                badges.push((fs.clone(), Color32::from_rgb(80, 100, 80)));
            }
        }
        if drive.is_encrypted {
            badges.push(("LUKS".into(), Color32::from_rgb(160, 90, 10)));
        }
        if drive.is_mounted() {
            badges.push(("mounted".into(), Color32::from_rgb(35, 130, 55)));
        }

        let mut bx = rect.min.x + px + dev_w + if dev_w > 0.0 { 8.0 } else { 0.0 };
        for (label, mut bg_color) in badges {
            if selected {
                bg_color = Color32::from_rgba_unmultiplied(255, 255, 255, 55);
            }
            let galley = painter.layout_no_wrap(
                label,
                egui::FontId::new(10.5, egui::FontFamily::Proportional),
                Color32::WHITE,
            );
            let bpad = egui::vec2(5.0, 2.0);
            let bsize = galley.size() + bpad * 2.0;
            let brect = egui::Rect::from_min_size(egui::pos2(bx, row2_y - 1.0), bsize);
            painter.rect_filled(brect, egui::CornerRadius::same(3), bg_color);
            painter.galley(brect.min + bpad, galley, Color32::WHITE);
            bx += bsize.x + 5.0;
        }
    }

    response
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

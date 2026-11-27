use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use eframe::egui;
use reelget_core::auth::AuthConfig;
use reelget_core::downloader::ProgressCallback;
use reelget_core::models::{
    ClipRange, DownloadOptions, DownloadResult, MediaInfo, Mp3Bitrate, OutputFormat, Platform,
    ProgressEvent, QualityPreference,
};
use reelget_core::ReelGetClient;
use tokio::runtime::Runtime;

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputMode {
    Single,
    Batch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QualityChoice {
    Best,
    P1080,
    P720,
    P480,
    P360,
    AudioOnly,
    CustomFormat(String),
}

impl QualityChoice {
    fn label(&self) -> String {
        match self {
            QualityChoice::Best => "Best Available".to_string(),
            QualityChoice::P1080 => "1080p (Full HD)".to_string(),
            QualityChoice::P720 => "720p (HD)".to_string(),
            QualityChoice::P480 => "480p (SD)".to_string(),
            QualityChoice::P360 => "360p".to_string(),
            QualityChoice::AudioOnly => "Audio Only".to_string(),
            QualityChoice::CustomFormat(id) => format!("Format: {id}"),
        }
    }

    fn to_preference(&self) -> QualityPreference {
        match self {
            QualityChoice::Best => QualityPreference::Best,
            QualityChoice::P1080 => QualityPreference::MaxHeight(1080),
            QualityChoice::P720 => QualityPreference::MaxHeight(720),
            QualityChoice::P480 => QualityPreference::MaxHeight(480),
            QualityChoice::P360 => QualityPreference::MaxHeight(360),
            QualityChoice::AudioOnly => QualityPreference::AudioOnly,
            QualityChoice::CustomFormat(id) => {
                QualityPreference::from_str(id).unwrap_or(QualityPreference::Best)
            }
        }
    }
}

#[derive(Debug, Clone)]
enum TaskStatus {
    Queued,
    Extracting,
    Downloading {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        completed_segments: Option<usize>,
        total_segments: Option<usize>,
    },
    Muxing(String),
    Completed {
        output_path: PathBuf,
        bytes_written: u64,
    },
    Failed(String),
}

#[derive(Debug, Clone)]
struct DownloadTask {
    id: usize,
    url: String,
    title: Option<String>,
    platform: Option<Platform>,
    status: TaskStatus,
}

#[derive(Debug)]
enum InspectState {
    Idle,
    Loading(String),
    Ready(MediaInfo),
    Error(String),
}

struct ThemePalette {
    panel_bg: egui::Color32,
    card_bg: egui::Color32,
    card_border: egui::Color32,
    text_primary: egui::Color32,
    text_secondary: egui::Color32,
    text_hint: egui::Color32,
    primary_btn_bg: egui::Color32,
    primary_btn_border: egui::Color32,
    secondary_btn_bg: egui::Color32,
    secondary_btn_border: egui::Color32,
    badge_bg: egui::Color32,
    badge_fg: egui::Color32,
    success_fg: egui::Color32,
    warning_fg: egui::Color32,
    error_fg: egui::Color32,
}

impl ThemePalette {
    fn for_mode(dark: bool) -> Self {
        if dark {
            Self {
                panel_bg: egui::Color32::from_rgb(15, 23, 42),
                card_bg: egui::Color32::from_rgb(30, 41, 59),
                card_border: egui::Color32::from_rgb(71, 85, 105),
                text_primary: egui::Color32::from_rgb(248, 250, 252),
                text_secondary: egui::Color32::from_rgb(203, 213, 225),
                text_hint: egui::Color32::from_rgb(148, 163, 184),
                primary_btn_bg: egui::Color32::from_rgb(37, 99, 235),
                primary_btn_border: egui::Color32::from_rgb(96, 165, 250),
                secondary_btn_bg: egui::Color32::from_rgb(51, 65, 85),
                secondary_btn_border: egui::Color32::from_rgb(100, 116, 139),
                badge_bg: egui::Color32::from_rgb(30, 58, 138),
                badge_fg: egui::Color32::from_rgb(219, 234, 254),
                success_fg: egui::Color32::from_rgb(74, 222, 128),
                warning_fg: egui::Color32::from_rgb(251, 191, 36),
                error_fg: egui::Color32::from_rgb(248, 113, 113),
            }
        } else {
            Self {
                panel_bg: egui::Color32::from_rgb(241, 245, 249),
                card_bg: egui::Color32::from_rgb(255, 255, 255),
                card_border: egui::Color32::from_rgb(148, 163, 184),
                text_primary: egui::Color32::from_rgb(15, 23, 42),
                text_secondary: egui::Color32::from_rgb(51, 65, 85),
                text_hint: egui::Color32::from_rgb(100, 116, 139),
                primary_btn_bg: egui::Color32::from_rgb(29, 78, 216),
                primary_btn_border: egui::Color32::from_rgb(30, 64, 175),
                secondary_btn_bg: egui::Color32::from_rgb(226, 232, 240),
                secondary_btn_border: egui::Color32::from_rgb(100, 116, 139),
                badge_bg: egui::Color32::from_rgb(219, 234, 254),
                badge_fg: egui::Color32::from_rgb(30, 58, 138),
                success_fg: egui::Color32::from_rgb(21, 128, 61),
                warning_fg: egui::Color32::from_rgb(180, 83, 9),
                error_fg: egui::Color32::from_rgb(185, 28, 28),
            }
        }
    }
}

pub struct ReelGetApp {
    runtime: Arc<Runtime>,
    client: Arc<ReelGetClient>,
    ffmpeg_path: Option<PathBuf>,

    // Safe-area insets & Android platform handles
    top_inset: f32,
    bottom_inset: f32,
    #[cfg(target_os = "android")]
    android_app: Option<winit::platform::android::activity::AndroidApp>,
    #[cfg(target_os = "android")]
    android_temp_dir: Option<PathBuf>,
    #[cfg(target_os = "android")]
    android_direct_writable: bool,
    #[cfg(target_os = "android")]
    last_checked_dir: String,

    // Theme & navigation state
    dark_mode: bool,
    show_exit_confirm: bool,

    // Input state
    input_mode: InputMode,
    single_url: String,
    batch_urls: String,
    custom_filename: String,
    output_dir: String,
    quality: QualityChoice,
    output_format: OutputFormat,
    mp3_bitrate: Mp3Bitrate,
    concurrency: usize,
    overwrite: bool,
    resume: bool,

    // Clip state (From - To in HH:MM:SS)
    clip_enabled: bool,
    clip_from_h: u32,
    clip_from_m: u32,
    clip_from_s: u32,
    clip_to_h: u32,
    clip_to_m: u32,
    clip_to_s: u32,
    clip_to_end: bool,

    // Auth / Private video state
    show_auth_panel: bool,
    cookies_file: String,
    raw_cookie: String,
    show_raw_cookie: bool,
    custom_headers_text: String,
    user_agent: String,

    // Inspect & Queue state
    inspect_state: Arc<Mutex<InspectState>>,
    tasks: Arc<Mutex<Vec<DownloadTask>>>,
    next_task_id: usize,
}

impl ReelGetApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to create Tokio runtime"),
        );
        let client = Arc::new(ReelGetClient::new());
        let ffmpeg_path = ReelGetClient::check_ffmpeg(None);
        let default_dir = default_download_dir();

        configure_fonts(&cc.egui_ctx);
        configure_theme(&cc.egui_ctx, true);

        Self {
            runtime,
            client,
            ffmpeg_path,
            top_inset: if cfg!(target_os = "android") { 36.0 } else { 0.0 },
            bottom_inset: if cfg!(target_os = "android") { 48.0 } else { 0.0 },
            #[cfg(target_os = "android")]
            android_app: None,
            #[cfg(target_os = "android")]
            android_temp_dir: None,
            #[cfg(target_os = "android")]
            android_direct_writable: false,
            #[cfg(target_os = "android")]
            last_checked_dir: String::new(),
            dark_mode: true,
            show_exit_confirm: false,
            input_mode: InputMode::Single,
            single_url: String::new(),
            batch_urls: String::new(),
            custom_filename: String::new(),
            output_dir: default_dir.to_string_lossy().to_string(),
            quality: QualityChoice::Best,
            output_format: OutputFormat::Original,
            mp3_bitrate: Mp3Bitrate::Kbps192,
            concurrency: 4,
            overwrite: false,
            resume: true,
            clip_enabled: false,
            clip_from_h: 0,
            clip_from_m: 0,
            clip_from_s: 0,
            clip_to_h: 0,
            clip_to_m: 0,
            clip_to_s: 15,
            clip_to_end: false,
            show_auth_panel: false,
            cookies_file: String::new(),
            raw_cookie: String::new(),
            show_raw_cookie: false,
            custom_headers_text: String::new(),
            user_agent: String::new(),
            inspect_state: Arc::new(Mutex::new(InspectState::Idle)),
            tasks: Arc::new(Mutex::new(Vec::new())),
            next_task_id: 1,
        }
    }

    /// Handle Android Back / Escape navigation step-by-step back to root, then confirm exit.
    pub fn handle_back_action(&mut self) {
        if self.show_exit_confirm {
            self.show_exit_confirm = false;
            return;
        }

        {
            let mut guard = self.inspect_state.lock().unwrap();
            if !matches!(*guard, InspectState::Idle) {
                *guard = InspectState::Idle;
                return;
            }
        }

        if self.show_auth_panel {
            self.show_auth_panel = false;
            return;
        }

        if self.clip_enabled {
            self.clip_enabled = false;
            return;
        }

        if self.input_mode == InputMode::Batch {
            self.input_mode = InputMode::Single;
            return;
        }

        self.show_exit_confirm = true;
    }

    fn has_active_downloads(&self) -> bool {
        let guard = self.tasks.lock().unwrap();
        guard.iter().any(|t| {
            matches!(
                t.status,
                TaskStatus::Queued
                    | TaskStatus::Extracting
                    | TaskStatus::Downloading { .. }
                    | TaskStatus::Muxing(_)
            )
        })
    }

    fn read_clipboard_text(&self) -> Option<String> {
        #[cfg(target_os = "android")]
        {
            if let Some(ref app) = self.android_app {
                return read_android_clipboard(app);
            }
            None
        }
        #[cfg(not(target_os = "android"))]
        {
            arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
    }

    fn build_auth_config(&self) -> Result<AuthConfig, String> {
        let mut headers = Vec::new();
        for line in self.custom_headers_text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let (k, v) = trimmed
                .split_once(':')
                .ok_or_else(|| format!("Invalid header '{trimmed}', expected 'Key: Value'"))?;
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }

        let cookies_file = {
            let t = self.cookies_file.trim();
            if t.is_empty() {
                None
            } else {
                Some(PathBuf::from(t))
            }
        };
        let raw_cookie = {
            let t = self.raw_cookie.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        let user_agent = {
            let t = self.user_agent.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };

        Ok(AuthConfig {
            cookies_file,
            raw_cookie,
            headers,
            user_agent,
        })
    }

    fn has_auth_configured(&self) -> bool {
        !self.cookies_file.trim().is_empty()
            || !self.raw_cookie.trim().is_empty()
            || !self.custom_headers_text.trim().is_empty()
    }

    fn build_clip_range(&self) -> Result<Option<ClipRange>, String> {
        if !self.clip_enabled {
            return Ok(None);
        }
        let start = Some((self.clip_from_h, self.clip_from_m, self.clip_from_s));
        let end = if self.clip_to_end {
            None
        } else {
            Some((self.clip_to_h, self.clip_to_m, self.clip_to_s))
        };
        ClipRange::from_hms(start, end).map(Some)
    }

    fn start_inspect(&self, ctx: &egui::Context) {
        let url = self.single_url.trim().to_string();
        if url.is_empty() {
            *self.inspect_state.lock().unwrap() =
                InspectState::Error("Please enter a video or reel URL first.".to_string());
            return;
        }

        let auth = match self.build_auth_config() {
            Ok(a) => a,
            Err(e) => {
                *self.inspect_state.lock().unwrap() = InspectState::Error(e);
                return;
            }
        };

        *self.inspect_state.lock().unwrap() = InspectState::Loading(url.clone());
        let state = Arc::clone(&self.inspect_state);
        let client = Arc::clone(&self.client);
        let ctx = ctx.clone();

        self.runtime.spawn(async move {
            let res = client.extract_info(&url, auth, 3).await;
            let mut guard = state.lock().unwrap();
            *guard = match res {
                Ok(info) => InspectState::Ready(info),
                Err(e) => InspectState::Error(e.to_string()),
            };
            ctx.request_repaint();
        });
    }

    fn start_downloads(&mut self, ctx: &egui::Context) {
        let urls: Vec<String> = match self.input_mode {
            InputMode::Single => {
                let u = self.single_url.trim().to_string();
                if u.is_empty() {
                    Vec::new()
                } else {
                    vec![u]
                }
            }
            InputMode::Batch => self
                .batch_urls
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect(),
        };

        if urls.is_empty() {
            *self.inspect_state.lock().unwrap() =
                InspectState::Error("Please enter at least one URL to download.".to_string());
            return;
        }

        let auth = match self.build_auth_config() {
            Ok(a) => a,
            Err(e) => {
                *self.inspect_state.lock().unwrap() = InspectState::Error(e);
                return;
            }
        };

        let clip = match self.build_clip_range() {
            Ok(c) => c,
            Err(e) => {
                *self.inspect_state.lock().unwrap() = InspectState::Error(e);
                return;
            }
        };

        if clip.is_some()
            && self.output_format == OutputFormat::Original
            && self.ffmpeg_path.is_none()
        {
            *self.inspect_state.lock().unwrap() = InspectState::Error(
                "Video clipping requires ffmpeg. Please install ffmpeg first.".to_string(),
            );
            return;
        }

        let out_dir = {
            let t = self.output_dir.trim();
            if t.is_empty() {
                default_download_dir()
            } else {
                PathBuf::from(t)
            }
        };

        #[cfg(target_os = "android")]
        let use_mediastore_export = !is_dir_writable(&out_dir);
        #[cfg(not(target_os = "android"))]
        let use_mediastore_export = false;

        #[cfg(target_os = "android")]
        let effective_download_dir = if use_mediastore_export {
            self.android_temp_dir
                .clone()
                .unwrap_or_else(|| out_dir.clone())
        } else {
            out_dir.clone()
        };
        #[cfg(not(target_os = "android"))]
        let effective_download_dir = out_dir.clone();

        let single_mode = urls.len() == 1;
        let custom_name = if single_mode && !self.custom_filename.trim().is_empty() {
            Some(self.custom_filename.trim().to_string())
        } else {
            None
        };

        for url in urls {
            let task_id = self.next_task_id;
            self.next_task_id += 1;

            {
                let mut guard = self.tasks.lock().unwrap();
                guard.insert(
                    0,
                    DownloadTask {
                        id: task_id,
                        url: url.clone(),
                        title: None,
                        platform: None,
                        status: TaskStatus::Queued,
                    },
                );
            }

            let opts = DownloadOptions {
                output_dir: effective_download_dir.clone(),
                filename: custom_name.clone(),
                quality: self.quality.to_preference(),
                output_format: self.output_format,
                mp3_bitrate: self.mp3_bitrate,
                overwrite: self.overwrite || use_mediastore_export,
                resume: self.resume,
                concurrency: self.concurrency,
                retries: 3,
                ffmpeg_path: self.ffmpeg_path.clone(),
                clip: clip.clone(),
                auth: auth.clone(),
            };

            let tasks_arc = Arc::clone(&self.tasks);
            let client = Arc::clone(&self.client);
            let ctx_cb = ctx.clone();
            let ctx_done = ctx.clone();

            let cb: ProgressCallback = Arc::new(move |ev| {
                let mut guard = tasks_arc.lock().unwrap();
                if let Some(task) = guard.iter_mut().find(|t| t.id == task_id) {
                    match ev {
                        ProgressEvent::Extracting { .. } => {
                            task.status = TaskStatus::Extracting;
                        }
                        ProgressEvent::Extracted {
                            title, platform, ..
                        } => {
                            task.title = Some(title);
                            task.platform = Some(platform);
                        }
                        ProgressEvent::Started {
                            total_bytes,
                            total_segments,
                            ..
                        } => {
                            task.status = TaskStatus::Downloading {
                                downloaded_bytes: 0,
                                total_bytes,
                                completed_segments: total_segments.map(|_| 0),
                                total_segments,
                            };
                        }
                        ProgressEvent::Progress {
                            downloaded_bytes,
                            total_bytes,
                            completed_segments,
                            total_segments,
                        } => {
                            task.status = TaskStatus::Downloading {
                                downloaded_bytes,
                                total_bytes,
                                completed_segments,
                                total_segments,
                            };
                        }
                        ProgressEvent::Muxing { message } => {
                            task.status = TaskStatus::Muxing(message);
                        }
                        ProgressEvent::Completed {
                            output_path,
                            bytes_written,
                        } => {
                            if use_mediastore_export {
                                task.status = TaskStatus::Muxing(
                                    "Saving to Android Download folder...".to_string(),
                                );
                            } else {
                                task.status = TaskStatus::Completed {
                                    output_path,
                                    bytes_written,
                                };
                            }
                        }
                    }
                }
                ctx_cb.request_repaint();
            });

            let tasks_done = Arc::clone(&self.tasks);
            #[cfg(target_os = "android")]
            let android_app_clone = self.android_app.clone();
            #[cfg(target_os = "android")]
            let display_out_dir = out_dir.clone();

            self.runtime.spawn(async move {
                let res: reelget_core::error::Result<DownloadResult> =
                    client.download(&url, opts, Some(cb)).await;
                match res {
                    Ok(dl) => {
                        #[cfg(target_os = "android")]
                        if use_mediastore_export {
                            let file_name = dl
                                .output_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("video.mp4")
                                .to_string();
                            let export_res = if let Some(ref app) = android_app_clone {
                                export_to_android_mediastore(app, &dl.output_path, &file_name)
                            } else {
                                Err("Android context unavailable for MediaStore export".to_string())
                            };
                            let _ = tokio::fs::remove_file(&dl.output_path).await;
                            let mut guard = tasks_done.lock().unwrap();
                            if let Some(task) = guard.iter_mut().find(|t| t.id == task_id) {
                                match export_res {
                                    Ok(_) => {
                                        task.status = TaskStatus::Completed {
                                            output_path: display_out_dir.join(&file_name),
                                            bytes_written: dl.bytes_written,
                                        };
                                    }
                                    Err(err) => {
                                        task.status = TaskStatus::Failed(err);
                                    }
                                }
                            }
                        }
                        #[cfg(not(target_os = "android"))]
                        let _ = dl;
                    }
                    Err(e) => {
                        let mut guard = tasks_done.lock().unwrap();
                        if let Some(task) = guard.iter_mut().find(|t| t.id == task_id) {
                            task.status = TaskStatus::Failed(e.to_string());
                        }
                    }
                }
                ctx_done.request_repaint();
            });
        }
    }

    fn render_exit_confirm_modal(&mut self, ctx: &egui::Context, pal: &ThemePalette) {
        if !self.show_exit_confirm {
            return;
        }

        let active = self.has_active_downloads();
        egui::Area::new(egui::Id::new("exit_confirm_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_black_alpha(170),
                );
                ui.allocate_rect(screen, egui::Sense::click());
            });

        egui::Window::new("Exit ReelGet?")
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .fixed_size(egui::vec2((ctx.screen_rect().width() - 48.0).min(340.0), 0.0))
            .frame(
                egui::Frame::new()
                    .fill(pal.card_bg)
                    .stroke(egui::Stroke::new(1.5_f32, pal.card_border))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                let msg = if active {
                    "Downloads are still in progress. Are you sure you want to exit ReelGet?"
                } else {
                    "Are you sure you want to exit ReelGet?"
                };
                ui.label(egui::RichText::new(msg).size(14.0).color(pal.text_primary));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if secondary_button(ui, "Cancel", pal).clicked() {
                        self.show_exit_confirm = false;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let exit_btn = egui::Button::new(
                            egui::RichText::new("Exit")
                                .strong()
                                .size(14.0)
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 38, 38))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(248, 113, 113),
                        ))
                        .corner_radius(egui::CornerRadius::same(6));
                        if ui.add(exit_btn).clicked() {
                            self.show_exit_confirm = false;
                            #[cfg(target_os = "android")]
                            if let Some(ref app) = self.android_app {
                                finish_android_activity(app);
                            }
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });
    }
}

impl eframe::App for ReelGetApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render_ui(ctx);
    }
}

impl ReelGetApp {
    pub fn render_ui(&mut self, ctx: &egui::Context) {
        let back_pressed = ctx.input(|i| {
            i.key_pressed(egui::Key::BrowserBack) || i.key_pressed(egui::Key::Escape)
        });
        if back_pressed {
            self.handle_back_action();
        }

        let pal = ThemePalette::for_mode(self.dark_mode);
        let screen_w = ctx.screen_rect().width();
        let is_compact = screen_w < 640.0;

        let top_pad = (10.0 + self.top_inset).round() as i8;
        let side_pad: i8 = if is_compact { 10 } else { 14 };

        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(pal.card_bg)
                    .inner_margin(egui::Margin {
                        left: side_pad,
                        right: side_pad,
                        top: top_pad,
                        bottom: 10,
                    })
                    .stroke(egui::Stroke::new(1.0_f32, pal.card_border)),
            )
            .show(ctx, |ui| {
                let theme_label = if self.dark_mode {
                    "Light Mode"
                } else {
                    "Dark Mode"
                };
                let (ff_text, ff_color) = if self.ffmpeg_path.is_some() {
                    ("ffmpeg: Ready", pal.success_fg)
                } else if is_compact {
                    ("ffmpeg: N/A (Progressive/HLS)", pal.warning_fg)
                } else {
                    ("ffmpeg: Not found (Progressive/HLS)", pal.warning_fg)
                };
                let (auth_text, auth_color) = if self.has_auth_configured() {
                    ("Auth: Private + Public", pal.primary_btn_border)
                } else {
                    ("Auth: Public", pal.text_secondary)
                };

                if is_compact {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("ReelGet")
                                .strong()
                                .size(20.0)
                                .color(pal.text_primary),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if secondary_button(ui, theme_label, &pal).clicked() {
                                    self.dark_mode = !self.dark_mode;
                                    configure_theme(ctx, self.dark_mode);
                                }
                            },
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(auth_text)
                                .strong()
                                .size(12.0)
                                .color(auth_color),
                        );
                        ui.label(egui::RichText::new("•").size(12.0).color(pal.text_hint));
                        ui.label(
                            egui::RichText::new(ff_text)
                                .strong()
                                .size(12.0)
                                .color(ff_color),
                        );
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("ReelGet")
                                .strong()
                                .size(22.0)
                                .color(pal.text_primary),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Multiplatform Video & Reel Downloader")
                                .size(13.5)
                                .color(pal.text_secondary),
                        );

                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if secondary_button(ui, theme_label, &pal).clicked() {
                                    self.dark_mode = !self.dark_mode;
                                    configure_theme(ctx, self.dark_mode);
                                }
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(ff_text)
                                        .strong()
                                        .size(12.5)
                                        .color(ff_color),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(auth_text)
                                        .strong()
                                        .size(12.5)
                                        .color(auth_color),
                                );
                            },
                        );
                    });
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::same(side_pad)),
            )
            .show(ctx, |ui| {
                let max_content_w = ui.available_width();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(max_content_w);

                        // Supported platforms strip
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("Supported:")
                                    .strong()
                                    .size(12.0)
                                    .color(pal.text_secondary),
                            );
                            let platforms: &[&str] = if is_compact {
                                &[
                                    "Facebook",
                                    "Instagram",
                                    "TikTok",
                                    "YouTube",
                                    "X",
                                    "Threads",
                                    "HLS/MP4",
                                ]
                            } else {
                                &[
                                    "Facebook Reels/Watch",
                                    "Instagram Reels",
                                    "TikTok",
                                    "YouTube/Shorts",
                                    "X (Twitter)",
                                    "Threads",
                                    "HLS / Direct MP4",
                                ]
                            };
                            for p in platforms {
                                pill_badge(ui, p, pal.badge_bg, pal.badge_fg);
                            }
                        });

                        ui.add_space(8.0);

                        // Input Card
                        card_frame(&pal).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if tab_button(
                                    ui,
                                    "Single URL",
                                    self.input_mode == InputMode::Single,
                                    &pal,
                                )
                                .clicked()
                                {
                                    self.input_mode = InputMode::Single;
                                }
                                if tab_button(
                                    ui,
                                    "Batch URLs",
                                    self.input_mode == InputMode::Batch,
                                    &pal,
                                )
                                .clicked()
                                {
                                    self.input_mode = InputMode::Batch;
                                }
                            });
                            ui.add_space(6.0);
                            ui.separator();
                            ui.add_space(6.0);

                            match self.input_mode {
                                InputMode::Single => {
                                    if is_compact {
                                        ui.label(
                                            egui::RichText::new("Video / Reel URL:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        ui.add_space(4.0);
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(&mut self.single_url)
                                                .hint_text(
                                                    egui::RichText::new(
                                                        "Paste Facebook / IG / TikTok / YT / X / Threads URL",
                                                    )
                                                    .color(pal.text_hint),
                                                )
                                                .desired_width(f32::INFINITY),
                                        );
                                        if resp.lost_focus()
                                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                        {
                                            self.start_downloads(ctx);
                                        }
                                        ui.add_space(6.0);
                                        ui.horizontal_wrapped(|ui| {
                                            if secondary_button(ui, "Paste", &pal).clicked() {
                                                if let Some(txt) = self.read_clipboard_text() {
                                                    self.single_url = txt;
                                                }
                                            }
                                            if secondary_button(ui, "Clear", &pal).clicked() {
                                                self.single_url.clear();
                                            }
                                            if secondary_button(ui, "Inspect", &pal).clicked() {
                                                self.start_inspect(ctx);
                                            }
                                            if primary_button(ui, "Download", &pal).clicked() {
                                                self.start_downloads(ctx);
                                            }
                                        });
                                    } else {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("Video / Reel URL:")
                                                    .strong()
                                                    .color(pal.text_primary),
                                            );
                                            let input_w =
                                                (ui.available_width() - 315.0).max(180.0);
                                            let resp = ui.add(
                                                egui::TextEdit::singleline(&mut self.single_url)
                                                    .hint_text(
                                                        egui::RichText::new(
                                                            "https://www.instagram.com/reel/... or Facebook / TikTok / YouTube / X / Threads",
                                                        )
                                                        .color(pal.text_hint),
                                                    )
                                                    .desired_width(input_w),
                                            );
                                            if resp.lost_focus()
                                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            {
                                                self.start_downloads(ctx);
                                            }
                                            if secondary_button(ui, "Paste", &pal).clicked() {
                                                if let Some(txt) = self.read_clipboard_text() {
                                                    self.single_url = txt;
                                                }
                                            }
                                            if secondary_button(ui, "Clear", &pal).clicked() {
                                                self.single_url.clear();
                                            }
                                            if secondary_button(ui, "Inspect", &pal).clicked() {
                                                self.start_inspect(ctx);
                                            }
                                            if primary_button(ui, "Download", &pal).clicked() {
                                                self.start_downloads(ctx);
                                            }
                                        });
                                    }
                                }
                                InputMode::Batch => {
                                    ui.label(
                                        egui::RichText::new("Paste multiple URLs (one per line):")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    ui.add_space(4.0);
                                    ui.add(
                                        egui::TextEdit::multiline(&mut self.batch_urls)
                                            .desired_rows(4)
                                            .desired_width(f32::INFINITY)
                                            .hint_text(
                                                egui::RichText::new(
                                                    "https://www.facebook.com/reel/...\nhttps://www.instagram.com/reel/...\nhttps://www.tiktok.com/@.../video/...",
                                                )
                                                .color(pal.text_hint),
                                            ),
                                    );
                                    ui.add_space(6.0);
                                    ui.horizontal_wrapped(|ui| {
                                        if secondary_button(ui, "Paste", &pal).clicked() {
                                            if let Some(txt) = self.read_clipboard_text() {
                                                if !self.batch_urls.is_empty()
                                                    && !self.batch_urls.ends_with('\n')
                                                {
                                                    self.batch_urls.push('\n');
                                                }
                                                self.batch_urls.push_str(&txt);
                                            }
                                        }
                                        if secondary_button(ui, "Clear", &pal).clicked() {
                                            self.batch_urls.clear();
                                        }
                                        if primary_button(ui, "Download All", &pal).clicked() {
                                            self.start_downloads(ctx);
                                        }
                                    });
                                }
                            }
                        });

                        ui.add_space(10.0);

                        // Output & Quality Card
                        card_frame(&pal).show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("Output & Quality")
                                    .strong()
                                    .size(15.0)
                                    .color(pal.text_primary),
                            );
                            ui.add_space(6.0);

                            if is_compact {
                                ui.label(
                                    egui::RichText::new("Save to:")
                                        .strong()
                                        .color(pal.text_primary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.output_dir)
                                        .desired_width(f32::INFINITY),
                                );
                                #[cfg(target_os = "android")]
                                {
                                    if self.last_checked_dir != self.output_dir {
                                        self.last_checked_dir = self.output_dir.clone();
                                        self.android_direct_writable =
                                            is_dir_writable(&PathBuf::from(self.output_dir.trim()));
                                    }
                                    if !self.android_direct_writable {
                                        ui.add_space(4.0);
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new(
                                                    "Saves via Android MediaStore to Download",
                                                )
                                                .size(12.0)
                                                .color(pal.text_secondary),
                                            );
                                            if secondary_button(
                                                ui,
                                                "Allow Direct Access",
                                                &pal,
                                            )
                                            .clicked()
                                            {
                                                if let Some(ref app) = self.android_app {
                                                    open_android_manage_storage_settings(app);
                                                }
                                                self.last_checked_dir.clear();
                                            }
                                        });
                                    }
                                }
                                #[cfg(not(target_os = "android"))]
                                {
                                    ui.add_space(4.0);
                                    if secondary_button(ui, "Browse Folder...", &pal).clicked() {
                                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                            self.output_dir =
                                                folder.to_string_lossy().to_string();
                                        }
                                    }
                                }
                            } else {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("Save to:")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    let w = (ui.available_width() - 115.0).max(160.0);
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.output_dir)
                                            .desired_width(w),
                                    );
                                    #[cfg(not(target_os = "android"))]
                                    if secondary_button(ui, "Browse...", &pal).clicked() {
                                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                            self.output_dir =
                                                folder.to_string_lossy().to_string();
                                        }
                                    }
                                });
                            }

                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new("Output:")
                                        .strong()
                                        .color(pal.text_primary),
                                );
                                if tab_button(
                                    ui,
                                    "Original (Video)",
                                    self.output_format == OutputFormat::Original,
                                    &pal,
                                )
                                .clicked()
                                {
                                    self.output_format = OutputFormat::Original;
                                }
                                if tab_button(
                                    ui,
                                    "MP3 (Audio)",
                                    self.output_format == OutputFormat::Mp3,
                                    &pal,
                                )
                                .clicked()
                                {
                                    self.output_format = OutputFormat::Mp3;
                                }
                            });

                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                if self.output_format == OutputFormat::Mp3 {
                                    ui.label(
                                        egui::RichText::new("MP3 Bitrate:")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    egui::ComboBox::from_id_salt("mp3_bitrate_combo")
                                        .selected_text(
                                            egui::RichText::new(self.mp3_bitrate.label())
                                                .color(pal.text_primary),
                                        )
                                        .show_ui(ui, |ui| {
                                            for br in Mp3Bitrate::ALL {
                                                ui.selectable_value(
                                                    &mut self.mp3_bitrate,
                                                    br,
                                                    br.label(),
                                                );
                                            }
                                        });
                                } else {
                                    ui.label(
                                        egui::RichText::new("Quality:")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    egui::ComboBox::from_id_salt("quality_combo")
                                        .selected_text(
                                            egui::RichText::new(self.quality.label())
                                                .color(pal.text_primary),
                                        )
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.quality,
                                                QualityChoice::Best,
                                                "Best Available",
                                            );
                                            ui.selectable_value(
                                                &mut self.quality,
                                                QualityChoice::P1080,
                                                "1080p (Full HD)",
                                            );
                                            ui.selectable_value(
                                                &mut self.quality,
                                                QualityChoice::P720,
                                                "720p (HD)",
                                            );
                                            ui.selectable_value(
                                                &mut self.quality,
                                                QualityChoice::P480,
                                                "480p (SD)",
                                            );
                                            ui.selectable_value(
                                                &mut self.quality,
                                                QualityChoice::P360,
                                                "360p",
                                            );
                                            ui.selectable_value(
                                                &mut self.quality,
                                                QualityChoice::AudioOnly,
                                                "Audio Only",
                                            );
                                        });
                                }

                                if !is_compact && self.input_mode == InputMode::Single {
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new("Filename (optional):")
                                            .color(pal.text_primary),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.custom_filename)
                                            .hint_text(
                                                egui::RichText::new("auto from title")
                                                    .color(pal.text_hint),
                                            )
                                            .desired_width(160.0),
                                    );
                                }
                            });

                            if is_compact && self.input_mode == InputMode::Single {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("Filename (optional):")
                                        .strong()
                                        .color(pal.text_primary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.custom_filename)
                                        .hint_text(
                                            egui::RichText::new("auto from title")
                                                .color(pal.text_hint),
                                        )
                                        .desired_width(f32::INFINITY),
                                );
                            }

                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new("HLS Threads:").color(pal.text_primary),
                                );
                                ui.add(egui::DragValue::new(&mut self.concurrency).range(1..=16));
                                ui.checkbox(&mut self.resume, "Resume");
                                ui.checkbox(&mut self.overwrite, "Overwrite");
                            });

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(6.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.checkbox(
                                    &mut self.clip_enabled,
                                    egui::RichText::new("Clip Video (From - To)")
                                        .strong()
                                        .color(pal.text_primary),
                                );

                                if self.clip_enabled {
                                    if self.output_format == OutputFormat::Original
                                        && self.ffmpeg_path.is_none()
                                    {
                                        ui.label(
                                            egui::RichText::new("Requires ffmpeg (not found)")
                                                .strong()
                                                .size(12.5)
                                                .color(pal.warning_fg),
                                        );
                                    } else {
                                        match self.build_clip_range() {
                                            Ok(Some(range)) => {
                                                let start_s = range.start_secs.unwrap_or(0.0);
                                                let summary = match range.end_secs {
                                                    Some(end_s) => format!(
                                                        "Range: {} -> {} ({:.0}s)",
                                                        ClipRange::format_hms(start_s),
                                                        ClipRange::format_hms(end_s),
                                                        end_s - start_s
                                                    ),
                                                    None => format!(
                                                        "Range: {} -> End",
                                                        ClipRange::format_hms(start_s)
                                                    ),
                                                };
                                                ui.label(
                                                    egui::RichText::new(summary)
                                                        .strong()
                                                        .size(12.5)
                                                        .color(pal.success_fg),
                                                );
                                            }
                                            Ok(None) => {}
                                            Err(err) => {
                                                ui.label(
                                                    egui::RichText::new(err)
                                                        .strong()
                                                        .size(12.5)
                                                        .color(pal.error_fg),
                                                );
                                            }
                                        }
                                    }
                                }
                            });

                            if self.clip_enabled {
                                ui.add_space(4.0);
                                if is_compact {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("From:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        hms_picker(
                                            ui,
                                            &mut self.clip_from_h,
                                            &mut self.clip_from_m,
                                            &mut self.clip_from_s,
                                            true,
                                            &pal,
                                        );
                                    });
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            egui::RichText::new("To:    ")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        hms_picker(
                                            ui,
                                            &mut self.clip_to_h,
                                            &mut self.clip_to_m,
                                            &mut self.clip_to_s,
                                            !self.clip_to_end,
                                            &pal,
                                        );
                                        ui.checkbox(&mut self.clip_to_end, "To End");
                                    });
                                } else {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            egui::RichText::new("From:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        hms_picker(
                                            ui,
                                            &mut self.clip_from_h,
                                            &mut self.clip_from_m,
                                            &mut self.clip_from_s,
                                            true,
                                            &pal,
                                        );

                                        ui.add_space(14.0);
                                        ui.label(
                                            egui::RichText::new("To:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        hms_picker(
                                            ui,
                                            &mut self.clip_to_h,
                                            &mut self.clip_to_m,
                                            &mut self.clip_to_s,
                                            !self.clip_to_end,
                                            &pal,
                                        );

                                        ui.add_space(6.0);
                                        ui.checkbox(&mut self.clip_to_end, "To End");
                                    });
                                }
                            }
                        });

                        ui.add_space(10.0);

                        // Private / Authenticated Video Settings
                        card_frame(&pal).show(ui, |ui| {
                            let prefix = if self.show_auth_panel { "[-]" } else { "[+]" };
                            let label = if is_compact {
                                format!("{prefix} Private / Auth (Cookies & Headers)")
                            } else {
                                format!(
                                    "{prefix} Private / Authenticated Video Settings (Cookies & Headers)"
                                )
                            };
                            if wrapped_tab_button(ui, &label, self.show_auth_panel, &pal).clicked()
                            {
                                self.show_auth_panel = !self.show_auth_panel;
                            }

                            if self.show_auth_panel {
                                ui.add_space(6.0);
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(
                                        "For private/friends-only/protected videos, supply your own authorized session cookies (Netscape cookies.txt or inline Cookie header). DRM-protected streams are not supported.",
                                    )
                                    .size(12.5)
                                    .color(pal.text_secondary),
                                );
                                ui.add_space(6.0);

                                if is_compact {
                                    ui.label(
                                        egui::RichText::new("cookies.txt path:")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.cookies_file)
                                            .hint_text(
                                                egui::RichText::new("/path/to/cookies.txt")
                                                    .color(pal.text_hint),
                                            )
                                            .desired_width(f32::INFINITY),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new("Inline Cookie:")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.raw_cookie)
                                            .password(!self.show_raw_cookie)
                                            .hint_text(
                                                egui::RichText::new(
                                                    "sessionid=...; csrftoken=...",
                                                )
                                                .color(pal.text_hint),
                                            )
                                            .desired_width(f32::INFINITY),
                                    );
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut self.show_raw_cookie, "Show Cookie");
                                        if secondary_button(ui, "Paste Cookie", &pal).clicked() {
                                            if let Some(txt) = self.read_clipboard_text() {
                                                self.raw_cookie = txt;
                                            }
                                        }
                                    });
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new("User-Agent:")
                                            .strong()
                                            .color(pal.text_primary),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.user_agent)
                                            .hint_text(
                                                egui::RichText::new("Optional custom User-Agent")
                                                    .color(pal.text_hint),
                                            )
                                            .desired_width(f32::INFINITY),
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("cookies.txt:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        let w = (ui.available_width() - 185.0).max(140.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.cookies_file)
                                                .hint_text(
                                                    egui::RichText::new("/path/to/cookies.txt")
                                                        .color(pal.text_hint),
                                                )
                                                .desired_width(w),
                                        );
                                        #[cfg(not(target_os = "android"))]
                                        if secondary_button(ui, "Browse...", &pal).clicked() {
                                            if let Some(file) = rfd::FileDialog::new()
                                                .add_filter("Cookies", &["txt"])
                                                .pick_file()
                                            {
                                                self.cookies_file =
                                                    file.to_string_lossy().to_string();
                                            }
                                        }
                                        if secondary_button(ui, "Clear", &pal).clicked() {
                                            self.cookies_file.clear();
                                        }
                                    });

                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("Inline Cookie:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        let w = (ui.available_width() - 165.0).max(140.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.raw_cookie)
                                                .password(!self.show_raw_cookie)
                                                .hint_text(
                                                    egui::RichText::new(
                                                        "sessionid=...; csrftoken=... (or c_user=...; xs=...)",
                                                    )
                                                    .color(pal.text_hint),
                                                )
                                                .desired_width(w),
                                        );
                                        ui.checkbox(&mut self.show_raw_cookie, "Show");
                                    });

                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("User-Agent:")
                                                .strong()
                                                .color(pal.text_primary),
                                        );
                                        let w = (ui.available_width() - 100.0).max(140.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.user_agent)
                                                .hint_text(
                                                    egui::RichText::new(
                                                        "Optional custom User-Agent",
                                                    )
                                                    .color(pal.text_hint),
                                                )
                                                .desired_width(w),
                                        );
                                    });
                                }

                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Custom Headers (one 'Key: Value' per line):",
                                    )
                                    .strong()
                                    .color(pal.text_primary),
                                );
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.custom_headers_text)
                                        .desired_rows(2)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(
                                            egui::RichText::new("Authorization: Bearer ...")
                                                .color(pal.text_hint),
                                        ),
                                );
                            }
                        });

                        ui.add_space(10.0);

                        // Inspected Media Preview
                        let inspect_snapshot = {
                            let guard = self.inspect_state.lock().unwrap();
                            match &*guard {
                                InspectState::Idle => None,
                                InspectState::Loading(u) => {
                                    Some(Err((true, format!("Inspecting {u}..."))))
                                }
                                InspectState::Error(e) => Some(Err((false, e.clone()))),
                                InspectState::Ready(info) => Some(Ok(info.clone())),
                            }
                        };

                        if let Some(state) = inspect_snapshot {
                            card_frame(&pal).show(ui, |ui| match state {
                                Err((true, msg)) => {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spinner();
                                        ui.label(egui::RichText::new(msg).color(pal.text_primary));
                                    });
                                }
                                Err((false, err)) => {
                                    ui.label(
                                        egui::RichText::new(format!("Error: {err}"))
                                            .strong()
                                            .color(pal.error_fg),
                                    );
                                }
                                Ok(info) => {
                                    ui.horizontal_wrapped(|ui| {
                                        pill_badge(
                                            ui,
                                            &info.platform.to_string(),
                                            pal.badge_bg,
                                            pal.badge_fg,
                                        );
                                        ui.label(
                                            egui::RichText::new(&info.title)
                                                .strong()
                                                .size(15.0)
                                                .color(pal.text_primary),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        if let Some(ref up) = info.uploader {
                                            ui.label(
                                                egui::RichText::new(format!("Uploader: {up}"))
                                                    .size(12.5)
                                                    .color(pal.text_secondary),
                                            );
                                        }
                                        if let Some(dur) = info.duration_secs {
                                            ui.label(
                                                egui::RichText::new(format!("Duration: {dur:.1}s"))
                                                    .size(12.5)
                                                    .color(pal.text_secondary),
                                            );
                                        }
                                        ui.label(
                                            egui::RichText::new(if info.is_private_or_auth {
                                                "Mode: Authenticated"
                                            } else {
                                                "Mode: Public"
                                            })
                                            .size(12.5)
                                            .color(pal.text_secondary),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    ui.separator();
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Available Formats ({}) — tap a format to select:",
                                            info.formats.len()
                                        ))
                                        .strong()
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                    );
                                    ui.add_space(4.0);
                                    egui::ScrollArea::vertical()
                                        .id_salt("formats_scroll")
                                        .max_height(150.0)
                                        .show(ui, |ui| {
                                            for fmt in &info.formats {
                                                let res = match (fmt.width, fmt.height) {
                                                    (Some(w), Some(h)) => format!("{w}x{h}"),
                                                    _ => fmt
                                                        .quality_label
                                                        .clone()
                                                        .unwrap_or_else(|| "-".to_string()),
                                                };
                                                let br = fmt
                                                    .bitrate_bps
                                                    .map(|b| format!("{} kbps", b / 1000))
                                                    .unwrap_or_else(|| "-".to_string());
                                                let row_label = format!(
                                                    "{} | {} | {} | {} | {:?} | {:?}",
                                                    fmt.format_id,
                                                    fmt.ext,
                                                    res,
                                                    br,
                                                    fmt.protocol,
                                                    fmt.kind
                                                );
                                                let selected = matches!(
                                                    &self.quality,
                                                    QualityChoice::CustomFormat(id) if id == &fmt.format_id
                                                );
                                                if wrapped_tab_button(
                                                    ui,
                                                    &row_label,
                                                    selected,
                                                    &pal,
                                                )
                                                .clicked()
                                                {
                                                    self.quality = QualityChoice::CustomFormat(
                                                        fmt.format_id.clone(),
                                                    );
                                                }
                                            }
                                        });
                                }
                            });
                            ui.add_space(10.0);
                        }

                        // Download Queue
                        let tasks_snapshot = self.tasks.lock().unwrap().clone();
                        card_frame(&pal).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Downloads ({})",
                                        tasks_snapshot.len()
                                    ))
                                    .strong()
                                    .size(15.0)
                                    .color(pal.text_primary),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if secondary_button(ui, "Clear Finished", &pal).clicked() {
                                            let mut guard = self.tasks.lock().unwrap();
                                            guard.retain(|t| {
                                                !matches!(
                                                    t.status,
                                                    TaskStatus::Completed { .. }
                                                        | TaskStatus::Failed(_)
                                                )
                                            });
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(4.0);

                            if tasks_snapshot.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "No downloads yet. Paste a URL above and tap Download.",
                                    )
                                    .color(pal.text_secondary),
                                );
                            } else {
                                for task in &tasks_snapshot {
                                    egui::Frame::new()
                                        .inner_margin(egui::Margin::same(6))
                                        .show(ui, |ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                if let Some(plat) = task.platform {
                                                    pill_badge(
                                                        ui,
                                                        &plat.to_string(),
                                                        pal.badge_bg,
                                                        pal.badge_fg,
                                                    );
                                                }
                                                let heading =
                                                    task.title.as_deref().unwrap_or(&task.url);
                                                ui.label(
                                                    egui::RichText::new(heading)
                                                        .strong()
                                                        .color(pal.text_primary),
                                                );
                                            });
                                            ui.add_space(4.0);

                                            match &task.status {
                                                TaskStatus::Queued => {
                                                    ui.label(
                                                        egui::RichText::new("Queued...")
                                                            .color(pal.text_secondary),
                                                    );
                                                }
                                                TaskStatus::Extracting => {
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.spinner();
                                                        ui.label(
                                                            egui::RichText::new(
                                                                "Extracting media metadata...",
                                                            )
                                                            .color(pal.text_primary),
                                                        );
                                                    });
                                                }
                                                TaskStatus::Downloading {
                                                    downloaded_bytes,
                                                    total_bytes,
                                                    completed_segments,
                                                    total_segments,
                                                } => {
                                                    if let (Some(done), Some(total)) =
                                                        (completed_segments, total_segments)
                                                    {
                                                        let frac = if *total > 0 {
                                                            *done as f32 / *total as f32
                                                        } else {
                                                            0.0
                                                        };
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "HLS segments: {done}/{total} ({:.2} MB)",
                                                                *downloaded_bytes as f64
                                                                    / (1024.0 * 1024.0)
                                                            ))
                                                            .size(13.0)
                                                            .color(pal.text_primary),
                                                        );
                                                        ui.add(
                                                            egui::ProgressBar::new(frac)
                                                                .fill(pal.primary_btn_bg)
                                                                .desired_height(12.0),
                                                        );
                                                    } else if let Some(total) = total_bytes {
                                                        let frac = if *total > 0 {
                                                            *downloaded_bytes as f32 / *total as f32
                                                        } else {
                                                            0.0
                                                        };
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "{:.2} / {:.2} MB ({:.0}%)",
                                                                *downloaded_bytes as f64
                                                                    / (1024.0 * 1024.0),
                                                                *total as f64 / (1024.0 * 1024.0),
                                                                frac * 100.0
                                                            ))
                                                            .size(13.0)
                                                            .color(pal.text_primary),
                                                        );
                                                        ui.add(
                                                            egui::ProgressBar::new(frac)
                                                                .fill(pal.primary_btn_bg)
                                                                .desired_height(12.0),
                                                        );
                                                    } else {
                                                        ui.horizontal_wrapped(|ui| {
                                                            ui.spinner();
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "Downloading {:.2} MB...",
                                                                    *downloaded_bytes as f64
                                                                        / (1024.0 * 1024.0)
                                                                ))
                                                                .color(pal.text_primary),
                                                            );
                                                        });
                                                    }
                                                }
                                                TaskStatus::Muxing(msg) => {
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.spinner();
                                                        ui.label(
                                                            egui::RichText::new(msg)
                                                                .color(pal.text_primary),
                                                        );
                                                    });
                                                }
                                                TaskStatus::Completed {
                                                    output_path,
                                                    bytes_written,
                                                } => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "Completed ({:.2} MB): {}",
                                                            *bytes_written as f64
                                                                / (1024.0 * 1024.0),
                                                            output_path.display()
                                                        ))
                                                        .strong()
                                                        .color(pal.success_fg),
                                                    );
                                                    ui.add_space(4.0);
                                                    ui.horizontal_wrapped(|ui| {
                                                        if secondary_button(ui, "Copy Path", &pal)
                                                            .clicked()
                                                        {
                                                            ctx.copy_text(
                                                                output_path
                                                                    .to_string_lossy()
                                                                    .to_string(),
                                                            );
                                                        }
                                                        #[cfg(not(target_os = "android"))]
                                                        if secondary_button(
                                                            ui,
                                                            "Open Folder",
                                                            &pal,
                                                        )
                                                        .clicked()
                                                        {
                                                            if let Some(parent) =
                                                                output_path.parent()
                                                            {
                                                                open_folder_in_os(parent);
                                                            }
                                                        }
                                                    });
                                                }
                                                TaskStatus::Failed(err) => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "Failed: {err}"
                                                        ))
                                                        .strong()
                                                        .color(pal.error_fg),
                                                    );
                                                }
                                            }
                                        });
                                    ui.separator();
                                }
                            }
                        });

                        if self.bottom_inset > 0.0 {
                            ui.add_space(self.bottom_inset + 16.0);
                        }
                    });
            });

        self.render_exit_confirm_modal(ctx, &pal);
    }
}

fn hms_picker(
    ui: &mut egui::Ui,
    hours: &mut u32,
    minutes: &mut u32,
    seconds: &mut u32,
    enabled: bool,
    pal: &ThemePalette,
) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(hours)
                    .range(0..=99)
                    .speed(0.2)
                    .custom_formatter(|n, _| format!("{:02}", n as u32)),
            );
            ui.label(egui::RichText::new("h :").color(pal.text_secondary));
            ui.add(
                egui::DragValue::new(minutes)
                    .range(0..=59)
                    .speed(0.2)
                    .custom_formatter(|n, _| format!("{:02}", n as u32)),
            );
            ui.label(egui::RichText::new("m :").color(pal.text_secondary));
            ui.add(
                egui::DragValue::new(seconds)
                    .range(0..=59)
                    .speed(0.2)
                    .custom_formatter(|n, _| format!("{:02}", n as u32)),
            );
            ui.label(egui::RichText::new("s").color(pal.text_secondary));
        });
    });
}

fn card_frame(pal: &ThemePalette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.card_bg)
        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
}

fn pill_badge(ui: &mut egui::Ui, text: &str, bg: egui::Color32, fg: egui::Color32) {
    let btn = egui::Button::new(egui::RichText::new(text).strong().size(11.5).color(fg))
        .fill(bg)
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(4))
        .min_size(egui::vec2(0.0, 22.0))
        .sense(egui::Sense::hover());
    ui.add(btn);
}

fn primary_button(ui: &mut egui::Ui, text: &str, pal: &ThemePalette) -> egui::Response {
    let btn = egui::Button::new(
        egui::RichText::new(text)
            .strong()
            .size(14.0)
            .color(egui::Color32::WHITE),
    )
    .fill(pal.primary_btn_bg)
    .stroke(egui::Stroke::new(1.0_f32, pal.primary_btn_border))
    .corner_radius(egui::CornerRadius::same(6));
    ui.add(btn)
}

fn secondary_button(ui: &mut egui::Ui, text: &str, pal: &ThemePalette) -> egui::Response {
    let btn = egui::Button::new(
        egui::RichText::new(text)
            .strong()
            .size(13.5)
            .color(pal.text_primary),
    )
    .fill(pal.secondary_btn_bg)
    .stroke(egui::Stroke::new(1.0_f32, pal.secondary_btn_border))
    .corner_radius(egui::CornerRadius::same(6));
    ui.add(btn)
}

fn tab_button(
    ui: &mut egui::Ui,
    text: &str,
    selected: bool,
    pal: &ThemePalette,
) -> egui::Response {
    let (fill, border, fg) = if selected {
        (
            pal.primary_btn_bg,
            pal.primary_btn_border,
            egui::Color32::WHITE,
        )
    } else {
        (
            pal.secondary_btn_bg,
            pal.secondary_btn_border,
            pal.text_primary,
        )
    };
    let btn = egui::Button::new(egui::RichText::new(text).strong().size(13.5).color(fg))
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, border))
        .corner_radius(egui::CornerRadius::same(6));
    ui.add(btn)
}

fn wrapped_tab_button(
    ui: &mut egui::Ui,
    text: &str,
    selected: bool,
    pal: &ThemePalette,
) -> egui::Response {
    let (fill, border, fg) = if selected {
        (
            pal.primary_btn_bg,
            pal.primary_btn_border,
            egui::Color32::WHITE,
        )
    } else {
        (
            pal.secondary_btn_bg,
            pal.secondary_btn_border,
            pal.text_primary,
        )
    };
    let btn = egui::Button::new(egui::RichText::new(text).strong().size(13.0).color(fg))
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, border))
        .corner_radius(egui::CornerRadius::same(6))
        .wrap();
    ui.add(btn)
}

fn configure_fonts(ctx: &egui::Context) {
    let candidates: &[&str] = &[
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/SFNS.ttf",
        #[cfg(target_os = "windows")]
        "C:\\Windows\\Fonts\\segoeui.ttf",
        #[cfg(target_os = "windows")]
        "C:\\Windows\\Fonts\\arial.ttf",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        #[cfg(target_os = "android")]
        "/system/fonts/Roboto-Regular.ttf",
    ];

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "system_sans".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                family.insert(0, "system_sans".to_owned());
            }
            ctx.set_fonts(fonts);
            break;
        }
    }
}

fn configure_theme(ctx: &egui::Context, dark: bool) {
    ctx.set_visuals_of(egui::Theme::Dark, build_visuals(true));
    ctx.set_visuals_of(egui::Theme::Light, build_visuals(false));
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });

    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = if cfg!(target_os = "android") {
            egui::vec2(12.0, 8.0)
        } else {
            egui::vec2(12.0, 6.0)
        };
        style.spacing.interact_size.y = if cfg!(target_os = "android") {
            36.0
        } else {
            30.0
        };
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(12.5, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(22.0, egui::FontFamily::Proportional),
        );
    });
}

fn build_visuals(dark: bool) -> egui::Visuals {
    let pal = ThemePalette::for_mode(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    v.override_text_color = Some(pal.text_primary);
    v.panel_fill = pal.panel_bg;
    v.window_fill = pal.card_bg;
    v.faint_bg_color = pal.card_bg;
    v.extreme_bg_color = if dark {
        egui::Color32::from_rgb(11, 17, 32)
    } else {
        egui::Color32::WHITE
    };

    v.selection.bg_fill = pal.primary_btn_bg;
    v.selection.stroke = egui::Stroke::new(1.0_f32, pal.primary_btn_border);

    v.widgets.noninteractive.bg_fill = pal.card_bg;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, pal.card_border);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);

    v.widgets.inactive.bg_fill = pal.secondary_btn_bg;
    v.widgets.inactive.weak_bg_fill = pal.secondary_btn_bg;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, pal.secondary_btn_border);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);

    v.widgets.hovered.bg_fill = if dark {
        egui::Color32::from_rgb(71, 85, 105)
    } else {
        egui::Color32::from_rgb(203, 213, 225)
    };
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.5_f32, pal.primary_btn_border);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);

    v.widgets.active.bg_fill = pal.primary_btn_bg;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.5_f32, pal.primary_btn_border);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

    v
}

fn default_download_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        PathBuf::from("/storage/emulated/0/Download")
    }
    #[cfg(not(target_os = "android"))]
    {
        dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(target_os = "android")]
fn is_dir_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".reelget_write_probe");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(not(target_os = "android"))]
fn open_folder_in_os(dir: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

pub fn app_icon() -> Option<Arc<egui::IconData>> {
    let png_bytes = include_bytes!("../assets/icon.png");
    eframe::icon_data::from_png_bytes(png_bytes).ok().map(Arc::new)
}

#[cfg(target_os = "android")]
fn query_android_downloads_dir(app: &winit::platform::android::activity::AndroidApp) -> PathBuf {
    use jni::objects::{JObject, JString, JValue};
    use jni::JavaVM;

    let fallback = PathBuf::from("/storage/emulated/0/Download");
    let vm = match unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) } {
        Ok(v) => v,
        Err(_) => return fallback,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return fallback,
    };

    let res = (|| -> Option<PathBuf> {
        let env_cls = env.find_class("android/os/Environment").ok()?;
        let dir_type = env
            .get_static_field(&env_cls, "DIRECTORY_DOWNLOADS", "Ljava/lang/String;")
            .ok()?
            .l()
            .ok()?;
        let file_obj = env
            .call_static_method(
                &env_cls,
                "getExternalStoragePublicDirectory",
                "(Ljava/lang/String;)Ljava/io/File;",
                &[JValue::Object(&dir_type)],
            )
            .ok()?
            .l()
            .ok()?;
        if file_obj.is_null() {
            return None;
        }
        let path_obj = env
            .call_method(&file_obj, "getAbsolutePath", "()Ljava/lang/String;", &[])
            .ok()?
            .l()
            .ok()?;
        if path_obj.is_null() {
            return None;
        }
        let jstr = JString::from(path_obj);
        let path_str = env.get_string(&jstr).ok()?.to_string_lossy().to_string();
        if path_str.is_empty() {
            None
        } else {
            Some(PathBuf::from(path_str))
        }
    })();

    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    let _ = JObject::null();
    res.unwrap_or(fallback)
}

#[cfg(target_os = "android")]
fn query_android_insets(app: &winit::platform::android::activity::AndroidApp) -> (f32, f32) {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let fallback = (36.0_f32, 48.0_f32);
    let vm = match unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) } {
        Ok(v) => v,
        Err(_) => return fallback,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return fallback,
    };
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    if activity.is_null() {
        return fallback;
    }

    let res = (|| -> Option<(f32, f32)> {
        let resources = env
            .call_method(&activity, "getResources", "()Landroid/content/res/Resources;", &[])
            .ok()?
            .l()
            .ok()?;
        let metrics = env
            .call_method(
                &resources,
                "getDisplayMetrics",
                "()Landroid/util/DisplayMetrics;",
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        let density = env.get_field(&metrics, "density", "F").ok()?.f().ok()?;
        let density = if density > 0.5 { density } else { 1.0 };

        let mut read_dimen_dp = |name: &str, default_dp: f32| -> f32 {
            let Ok(jname) = env.new_string(name) else {
                return default_dp;
            };
            let Ok(jtype) = env.new_string("dimen") else {
                return default_dp;
            };
            let Ok(jpkg) = env.new_string("android") else {
                return default_dp;
            };
            let id = env
                .call_method(
                    &resources,
                    "getIdentifier",
                    "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
                    &[
                        JValue::Object(&jname),
                        JValue::Object(&jtype),
                        JValue::Object(&jpkg),
                    ],
                )
                .ok()
                .and_then(|v| v.i().ok())
                .unwrap_or(0);
            if id <= 0 {
                return default_dp;
            }
            let px = env
                .call_method(
                    &resources,
                    "getDimensionPixelSize",
                    "(I)I",
                    &[JValue::Int(id)],
                )
                .ok()
                .and_then(|v| v.i().ok())
                .unwrap_or(0);
            if px > 0 {
                (px as f32) / density
            } else {
                default_dp
            }
        };

        let top = read_dimen_dp("status_bar_height", 36.0).clamp(24.0, 64.0);
        let bottom = read_dimen_dp("navigation_bar_height", 48.0).clamp(24.0, 72.0);
        Some((top, bottom))
    })();

    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    std::mem::forget(activity);
    res.unwrap_or(fallback)
}

#[cfg(target_os = "android")]
fn read_android_clipboard(app: &winit::platform::android::activity::AndroidApp) -> Option<String> {
    use jni::objects::{JObject, JString, JValue};
    use jni::JavaVM;

    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    if activity.is_null() {
        return None;
    }

    let res = (|| -> Option<String> {
        let looper_cls = env.find_class("android/os/Looper").ok()?;
        let my_looper = env
            .call_static_method(&looper_cls, "myLooper", "()Landroid/os/Looper;", &[])
            .ok()?
            .l()
            .ok()?;
        if my_looper.is_null() {
            let _ = env.call_static_method(&looper_cls, "prepare", "()V", &[]);
        }

        let svc_name = env.new_string("clipboard").ok()?;
        let cm = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&svc_name)],
            )
            .ok()?
            .l()
            .ok()?;
        if cm.is_null() {
            return None;
        }

        let clip = env
            .call_method(
                &cm,
                "getPrimaryClip",
                "()Landroid/content/ClipData;",
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        if clip.is_null() {
            return None;
        }

        let count = env
            .call_method(&clip, "getItemCount", "()I", &[])
            .ok()?
            .i()
            .ok()?;
        if count <= 0 {
            return None;
        }

        let item = env
            .call_method(
                &clip,
                "getItemAt",
                "(I)Landroid/content/ClipData$Item;",
                &[JValue::Int(0)],
            )
            .ok()?
            .l()
            .ok()?;
        if item.is_null() {
            return None;
        }

        let cs = env
            .call_method(
                &item,
                "coerceToText",
                "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                &[JValue::Object(&activity)],
            )
            .ok()?
            .l()
            .ok()?;
        if cs.is_null() {
            return None;
        }

        let jstr_obj = env
            .call_method(&cs, "toString", "()Ljava/lang/String;", &[])
            .ok()?
            .l()
            .ok()?;
        if jstr_obj.is_null() {
            return None;
        }

        let jstr = JString::from(jstr_obj);
        let text = env.get_string(&jstr).ok()?.to_string_lossy().trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    })();

    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    std::mem::forget(activity);
    res
}

#[cfg(target_os = "android")]
fn export_to_android_mediastore(
    app: &winit::platform::android::activity::AndroidApp,
    src_path: &Path,
    filename: &str,
) -> Result<(), String> {
    use std::io::Read;
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }
        .map_err(|e| format!("JavaVM error: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("JNI attach error: {e}"))?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    if activity.is_null() {
        return Err("Android activity is null".to_string());
    }

    let result = (|| -> Result<(), String> {
        let resolver = env
            .call_method(
                &activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| format!("getContentResolver failed: {e}"))?
            .l()
            .map_err(|e| format!("getContentResolver obj failed: {e}"))?;

        let downloads_cls = env
            .find_class("android/provider/MediaStore$Downloads")
            .map_err(|e| format!("MediaStore.Downloads requires Android 10+: {e}"))?;
        let collection_uri = env
            .get_static_field(&downloads_cls, "EXTERNAL_CONTENT_URI", "Landroid/net/Uri;")
            .map_err(|e| format!("EXTERNAL_CONTENT_URI failed: {e}"))?
            .l()
            .map_err(|e| format!("EXTERNAL_CONTENT_URI obj failed: {e}"))?;

        let cv_cls = env
            .find_class("android/content/ContentValues")
            .map_err(|e| format!("ContentValues class failed: {e}"))?;
        let cv = env
            .new_object(&cv_cls, "()V", &[])
            .map_err(|e| format!("ContentValues init failed: {e}"))?;

        let mime = match src_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4")
            .to_ascii_lowercase()
            .as_str()
        {
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "mkv" => "video/x-matroska",
            "ts" => "video/mp2t",
            "m4a" => "audio/mp4",
            "mp3" => "audio/mpeg",
            _ => "video/mp4",
        };

        let k_name = env.new_string("_display_name").map_err(|e| e.to_string())?;
        let v_name = env.new_string(filename).map_err(|e| e.to_string())?;
        env.call_method(
            &cv,
            "put",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&k_name), JValue::Object(&v_name)],
        )
        .map_err(|e| e.to_string())?;

        let k_mime = env.new_string("mime_type").map_err(|e| e.to_string())?;
        let v_mime = env.new_string(mime).map_err(|e| e.to_string())?;
        env.call_method(
            &cv,
            "put",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&k_mime), JValue::Object(&v_mime)],
        )
        .map_err(|e| e.to_string())?;

        let k_rel = env.new_string("relative_path").map_err(|e| e.to_string())?;
        let v_rel = env.new_string("Download").map_err(|e| e.to_string())?;
        env.call_method(
            &cv,
            "put",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&k_rel), JValue::Object(&v_rel)],
        )
        .map_err(|e| e.to_string())?;

        let int_cls = env.find_class("java/lang/Integer").map_err(|e| e.to_string())?;
        let one_obj = env
            .call_static_method(&int_cls, "valueOf", "(I)Ljava/lang/Integer;", &[JValue::Int(1)])
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        let k_pend = env.new_string("is_pending").map_err(|e| e.to_string())?;
        env.call_method(
            &cv,
            "put",
            "(Ljava/lang/String;Ljava/lang/Integer;)V",
            &[JValue::Object(&k_pend), JValue::Object(&one_obj)],
        )
        .map_err(|e| e.to_string())?;

        let item_uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&collection_uri), JValue::Object(&cv)],
            )
            .map_err(|e| format!("MediaStore insert failed: {e}"))?
            .l()
            .map_err(|e| e.to_string())?;
        if item_uri.is_null() {
            return Err("MediaStore insert returned null URI".to_string());
        }

        let out_stream = env
            .call_method(
                &resolver,
                "openOutputStream",
                "(Landroid/net/Uri;)Ljava/io/OutputStream;",
                &[JValue::Object(&item_uri)],
            )
            .map_err(|e| format!("openOutputStream failed: {e}"))?
            .l()
            .map_err(|e| e.to_string())?;
        if out_stream.is_null() {
            return Err("openOutputStream returned null".to_string());
        }

        let mut file = std::fs::File::open(src_path)
            .map_err(|e| format!("Failed to open staged file: {e}"))?;
        let mut buf = vec![0u8; 64 * 1024];
        let jbuf = env
            .new_byte_array(buf.len() as i32)
            .map_err(|e| e.to_string())?;

        loop {
            let n = file.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let signed: &[i8] =
                unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const i8, n) };
            env.set_byte_array_region(&jbuf, 0, signed)
                .map_err(|e| e.to_string())?;
            env.call_method(
                &out_stream,
                "write",
                "([BII)V",
                &[
                    JValue::Object(&jbuf),
                    JValue::Int(0),
                    JValue::Int(n as i32),
                ],
            )
            .map_err(|e| format!("OutputStream write failed: {e}"))?;
        }

        let _ = env.call_method(&out_stream, "flush", "()V", &[]);
        let _ = env.call_method(&out_stream, "close", "()V", &[]);

        let _ = env.call_method(&cv, "clear", "()V", &[]);
        let zero_obj = env
            .call_static_method(&int_cls, "valueOf", "(I)Ljava/lang/Integer;", &[JValue::Int(0)])
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        env.call_method(
            &cv,
            "put",
            "(Ljava/lang/String;Ljava/lang/Integer;)V",
            &[JValue::Object(&k_pend), JValue::Object(&zero_obj)],
        )
        .map_err(|e| e.to_string())?;
        let _ = env.call_method(
            &resolver,
            "update",
            "(Landroid/net/Uri;Landroid/content/ContentValues;Ljava/lang/String;[Ljava/lang/String;)I",
            &[
                JValue::Object(&item_uri),
                JValue::Object(&cv),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
            ],
        );

        Ok(())
    })();

    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    std::mem::forget(activity);
    result
}

#[cfg(target_os = "android")]
fn open_android_manage_storage_settings(app: &winit::platform::android::activity::AndroidApp) {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let Ok(vm) = (unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }) else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    if activity.is_null() {
        return;
    }

    let _ = (|| -> Option<()> {
        let uri_cls = env.find_class("android/net/Uri").ok()?;
        let pkg_str = env.new_string("package:com.reelget.app").ok()?;
        let uri = env
            .call_static_method(
                &uri_cls,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&pkg_str)],
            )
            .ok()?
            .l()
            .ok()?;

        let intent_cls = env.find_class("android/content/Intent").ok()?;
        let action = env
            .new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")
            .ok()?;
        let intent = env
            .new_object(
                &intent_cls,
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[JValue::Object(&action), JValue::Object(&uri)],
            )
            .ok()?;
        let _ = env.call_method(
            &activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        );
        Some(())
    })();

    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    std::mem::forget(activity);
}

#[cfg(target_os = "android")]
fn finish_android_activity(app: &winit::platform::android::activity::AndroidApp) {
    use jni::objects::JObject;
    use jni::JavaVM;

    let Ok(vm) = (unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }) else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    if activity.is_null() {
        return;
    }
    let _ = env.call_method(&activity, "finish", "()V", &[]);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    std::mem::forget(activity);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let downloads_dir = query_android_downloads_dir(&app);
    let temp_dir = app
        .external_data_path()
        .or_else(|| app.internal_data_path())
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    let (top_inset, bottom_inset) = query_android_insets(&app);
    let app_handle = app.clone();

    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    options.android_app = Some(app);
    let _ = eframe::run_native(
        "ReelGet",
        options,
        Box::new(move |cc| {
            let mut gui = ReelGetApp::new(cc);
            let dl_str = downloads_dir.to_string_lossy().to_string();
            gui.android_direct_writable = is_dir_writable(&downloads_dir);
            gui.last_checked_dir = dl_str.clone();
            gui.output_dir = dl_str;
            gui.top_inset = top_inset;
            gui.bottom_inset = bottom_inset;
            gui.android_app = Some(app_handle);
            gui.android_temp_dir = Some(temp_dir);
            Ok(Box::new(gui))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_icon_decodes() {
        let icon = app_icon().expect("embedded icon.png should decode");
        assert_eq!(icon.width, 512);
        assert_eq!(icon.height, 512);
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    }

    #[test]
    fn test_back_navigation_stack_and_exit_confirm() {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let mut app = ReelGetApp {
            runtime,
            client: Arc::new(ReelGetClient::new()),
            ffmpeg_path: None,
            top_inset: 36.0,
            bottom_inset: 48.0,
            #[cfg(target_os = "android")]
            android_app: None,
            #[cfg(target_os = "android")]
            android_temp_dir: None,
            #[cfg(target_os = "android")]
            android_direct_writable: false,
            #[cfg(target_os = "android")]
            last_checked_dir: String::new(),
            dark_mode: true,
            show_exit_confirm: false,
            input_mode: InputMode::Batch,
            single_url: String::new(),
            batch_urls: String::new(),
            custom_filename: String::new(),
            output_dir: "/storage/emulated/0/Download".to_string(),
            quality: QualityChoice::Best,
            output_format: OutputFormat::Original,
            mp3_bitrate: Mp3Bitrate::Kbps192,
            concurrency: 4,
            overwrite: false,
            resume: true,
            clip_enabled: true,
            clip_from_h: 0,
            clip_from_m: 0,
            clip_from_s: 0,
            clip_to_h: 0,
            clip_to_m: 0,
            clip_to_s: 15,
            clip_to_end: false,
            show_auth_panel: true,
            cookies_file: String::new(),
            raw_cookie: String::new(),
            show_raw_cookie: false,
            custom_headers_text: String::new(),
            user_agent: String::new(),
            inspect_state: Arc::new(Mutex::new(InspectState::Error("err".to_string()))),
            tasks: Arc::new(Mutex::new(Vec::new())),
            next_task_id: 1,
        };

        // 1. Back closes Inspect card
        app.handle_back_action();
        assert!(matches!(*app.inspect_state.lock().unwrap(), InspectState::Idle));
        assert!(!app.show_exit_confirm);

        // 2. Back closes Auth panel
        app.handle_back_action();
        assert!(!app.show_auth_panel);
        assert!(!app.show_exit_confirm);

        // 3. Back closes Clip panel
        app.handle_back_action();
        assert!(!app.clip_enabled);
        assert!(!app.show_exit_confirm);

        // 4. Back switches Batch -> Single
        app.handle_back_action();
        assert_eq!(app.input_mode, InputMode::Single);
        assert!(!app.show_exit_confirm);

        // 5. Back at root opens Confirm Exit dialog
        app.handle_back_action();
        assert!(app.show_exit_confirm);

        // 6. Back while Confirm Exit is open dismisses it
        app.handle_back_action();
        assert!(!app.show_exit_confirm);
    }

    #[test]
    fn test_mobile_layout_fits_width() {
        let ctx = egui::Context::default();
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let mut app = ReelGetApp {
            runtime,
            client: Arc::new(ReelGetClient::new()),
            ffmpeg_path: None,
            top_inset: 36.0,
            bottom_inset: 48.0,
            #[cfg(target_os = "android")]
            android_app: None,
            #[cfg(target_os = "android")]
            android_temp_dir: None,
            #[cfg(target_os = "android")]
            android_direct_writable: false,
            #[cfg(target_os = "android")]
            last_checked_dir: String::new(),
            dark_mode: true,
            show_exit_confirm: false,
            input_mode: InputMode::Single,
            single_url: String::new(),
            batch_urls: String::new(),
            custom_filename: String::new(),
            output_dir: "/storage/emulated/0/Download".to_string(),
            quality: QualityChoice::Best,
            output_format: OutputFormat::Original,
            mp3_bitrate: Mp3Bitrate::Kbps192,
            concurrency: 4,
            overwrite: false,
            resume: true,
            clip_enabled: true,
            clip_from_h: 0,
            clip_from_m: 0,
            clip_from_s: 0,
            clip_to_h: 0,
            clip_to_m: 0,
            clip_to_s: 15,
            clip_to_end: false,
            show_auth_panel: true,
            cookies_file: String::new(),
            raw_cookie: String::new(),
            show_raw_cookie: false,
            custom_headers_text: String::new(),
            user_agent: String::new(),
            inspect_state: Arc::new(Mutex::new(InspectState::Idle)),
            tasks: Arc::new(Mutex::new(Vec::new())),
            next_task_id: 1,
        };

        configure_theme(&ctx, true);
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(390.0, 844.0),
            )),
            ..Default::default()
        };

        let mut last_output = None;
        for _ in 0..2 {
            last_output = Some(ctx.run(raw_input.clone(), |ctx| {
                app.render_ui(ctx);
            }));
        }
        let used = ctx.used_rect();
        assert!(
            used.width() <= 391.0,
            "Mobile layout overflowed width: {} > 391.0",
            used.width()
        );
        if let Some(out) = last_output {
            assert!(!out.shapes.is_empty());
            let mut min_y = f32::MAX;
            let mut max_y = f32::MIN;
            for s in &out.shapes {
                let b = s.shape.visual_bounding_rect();
                if b.is_positive() {
                    min_y = min_y.min(b.min.y);
                    max_y = max_y.max(b.max.y);
                }
            }
            assert!(min_y < 60.0, "Top content started too low: {min_y}");
            assert!(max_y > 400.0, "Content height unexpectedly small: {max_y}");
        }
    }
}

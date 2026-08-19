//! eframe GUI v2 - 现代设计。
//!
//! eframe 0.32 仍使用 deprecated `Rounding` / `Frame::none()` / `menu::bar`，
//! egui 0.33+ 改名为 `CornerRadius` / `Frame::new()` / `MenuBar::new().ui(...)`。
//! 待升级到 egui 0.33 时统一迁移。
#![allow(deprecated, float_literal_f32_fallback)]
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use glow::HasContext;
use last_tftp_core::client::{Client, Progress};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tab {
    Server,
    Client,
    Transfers,
    Settings,
}

impl Default for Tab {
    fn default() -> Self {
        Tab::Client
    }
}

#[derive(Debug, Clone)]
pub enum Direction {
    Get,
    Put,
}

#[derive(Debug, Clone)]
pub struct TransferState {
    pub id: u64,
    pub direction: Direction,
    pub target: String,
    pub bytes: u64,
    pub blocks: u64,
    pub total: Option<u64>,
    pub started: Instant,
    pub done: bool,
    pub error: Option<String>,
    pub bps_ema: f64,
}

#[derive(Debug)]
pub enum GuiMsg {
    Progress {
        id: u64,
        bytes: u64,
        blocks: u64,
        total: Option<u64>,
    },
    Done {
        id: u64,
        bytes: u64,
        error: Option<String>,
    },
}

#[allow(dead_code)]
pub fn run() -> anyhow::Result<()> {
    run_with_options(None)
}

#[doc(hidden)]
pub fn run_with_options(
    auto_get: Option<(String, u16, String, PathBuf)>,
) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    let icon_data = load_icon_from_resources();
    #[cfg(not(target_os = "windows"))]
    let icon_data: Option<egui::IconData> = None;

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("last-tftp")
        .with_inner_size([960.0, 640.0])
        .with_min_inner_size([760.0, 480.0]);
    if let Some(icon) = icon_data {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "last-tftp",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let gl = cc.gl.clone();
            let mut app = App::new(gl);
            app.auto_get = auto_get.clone();
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

/// 从嵌入的 PNG 数据加载窗口图标。
#[cfg(target_os = "windows")]
fn load_icon_from_resources() -> Option<egui::IconData> {
    // 嵌入编译时的 PNG 图标数据
    let png_data: &[u8] = include_bytes!("../../../resources/icons/icon_48x48.png");
    let img = image::load_from_memory(png_data).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    let rgba = img.into_raw();
    Some(egui::IconData { rgba, width, height })
}

#[derive(Default)]
pub struct ServerFields {
    pub root: PathBuf,
    pub port: u16,
    pub allow_write: bool,
    pub running: bool,
}

pub struct App {
    pub server: ServerFields,
    pub server_root_str: String,
    pub server_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub server_status_rx: Option<Receiver<String>>,
    pub server_transfer_rx: Option<Receiver<ServerTransferEvent>>,
    pub server_progress_rx: Option<Receiver<(u64, Option<u64>)>>,
    pub remote_host: String,
    pub remote_port: u16,
    pub blksize: u16,
    pub window: u16,
    pub ipv6: bool,
    pub remote_file: String,
    pub local_file: PathBuf,
    pub local_file_str: String,
    pub next_id: u64,
    pub transfers: Vec<TransferState>,
    pub msg_tx: Sender<GuiMsg>,
    pub msg_rx: Receiver<GuiMsg>,
    pub log: Vec<String>,
    pub gl: Option<Arc<glow::Context>>,
    /// 本机所有接口的 IP，server 启动时显示
    pub local_ips: Vec<std::net::IpAddr>,
    pub screenshot_requested: std::sync::atomic::AtomicBool,
    pub viewport_size: (u32, u32),
    pub auto_get: Option<(String, u16, String, PathBuf)>,
    pub tab: Tab,
    pub last_action: LastAction,
}

impl App {
    #[allow(dead_code)] // start_transfer_*_for_test 仅 tests/ 集成测试用到
    fn new(gl: Option<Arc<glow::Context>>) -> Self {
        let mut app = Self::headless_new();
        app.gl = gl;
        app
    }

    #[doc(hidden)]
    pub fn headless_new() -> Self {
        let (tx, rx) = channel();
        Self {
            server: ServerFields {
                root: PathBuf::from("."),
                port: 69,
                allow_write: false,
                running: false,
            },
            server_root_str: String::from("."),
            server_stop: None,
            remote_host: String::from("127.0.0.1"),
            remote_port: 69,
            blksize: 1468,
            window: 1,
            ipv6: false,
            remote_file: String::new(),
            local_file: PathBuf::new(),
            local_file_str: String::new(),
            next_id: 1,
            transfers: Vec::new(),

            server_status_rx: None,
            server_transfer_rx: None,
            server_progress_rx: None,
            gl: None,
            local_ips: collect_local_ips(),
            msg_tx: tx,
            msg_rx: rx,
            log: Vec::new(),
            screenshot_requested: std::sync::atomic::AtomicBool::new(false),
            viewport_size: (1920, 1200),
            auto_get: None,
            tab: Tab::default(),
            last_action: LastAction::default(),
        }
    }

    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn start_transfer_get_for_test(&mut self) {
        self.start_transfer(Direction::Get);
    }

    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn start_transfer_put_for_test(&mut self) {
        self.start_transfer(Direction::Put);
    }

    /// 弹出文件/目录选择器。
    /// 规则：
    /// - GET（下载）：选个**目录**即可，最终路径 = 目录 + Remote 文件名。
    /// - PUT（上传）：必须选**文件**。
    /// 通过当前激活按钮（self.last_action）判断方向，未点过按钮时按 Remote 是否已填猜。
    fn pick_local_path(&mut self) {
        let start = self.local_file.parent().unwrap_or_else(|| std::path::Path::new("."));
        let name = self.local_file.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let is_get = self.last_action_is_get();
        if is_get {
            // GET：选目录作为下载目标
            if let Some(dir) = rfd::FileDialog::new()
                .set_directory(start)
                .pick_folder()
            {
                let remote = self.remote_file.trim();
                let full = if remote.is_empty() {
                    dir
                } else {
                    dir.join(remote)
                };
                self.local_file_str = full.display().to_string();
            }
        } else {
            // PUT：选源文件
            if let Some(path) = rfd::FileDialog::new()
                .set_directory(start)
                .set_file_name(name)
                .pick_file()
            {
                self.local_file_str = path.display().to_string();
            }
        }
    }

    /// 推断当前是 GET 还是 PUT 模式。
    fn last_action_is_get(&self) -> bool {
        self.last_action == LastAction::Get
    }

    fn push_log(&mut self, line: impl Into<String>) {
        let line = line.into();
        self.log.push(line.clone());
        if self.log.len() > 500 {
            self.log.remove(0);
        }
        if let Some(path) = Self::log_file_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{}", &line);
            }
        }
    }
    fn log_file_path() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("last-tftp.log"))
    }

    fn start_server(&mut self) {
        if self.server.running {
            return;
        }
        self.server.root = PathBuf::from(self.server_root_str.clone());
        let root = self.server.root.clone();
        let port = self.server.port;
        let allow_write = self.server.allow_write;
        let ipv6 = self.ipv6;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_self = Arc::clone(&stop);
        let (st_tx, st_rx) = std::sync::mpsc::channel::<String>();
        let (tr_tx, tr_rx) = std::sync::mpsc::channel::<ServerTransferEvent>();
        let (pr_tx, pr_rx) = std::sync::mpsc::channel::<(u64, Option<u64>)>();
        self.server_transfer_rx = Some(tr_rx);
        self.server_progress_rx = Some(pr_rx);

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let bind = if ipv6 {
                    format!("[::]:{port}")
                } else {
                    format!("0.0.0.0:{port}")
                };
                let addr: std::net::SocketAddr = match bind.parse() {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = st_tx.send(format!("server: bind parse: {e}"));
                        return;
                    }
                };
                // 显式 bind 一次并立刻 push listening 状态，避免 serve() 阻塞时 UI 不知。
                let probe = tokio::net::UdpSocket::bind(addr).await;
                match probe {
                    Ok(s) => {
                        drop(s);
                        let _ = st_tx.send(format!("server: listening on {addr}"));
                    }
                    Err(e) => {
                        let _ = st_tx.send(format!("server: bind {addr} failed: {e}"));
                        return;
                    }
                }
                let cfg = last_tftp_core::server::ServerConfig::new(root, allow_write);
                if let Err(e) = run_server_with_observer(addr, cfg, tr_tx.clone(), pr_tx, std::sync::Arc::clone(&stop)).await {
                    let _ = st_tx.send(format!("server stopped: {e}"));
                }
            });
        });
        std::thread::spawn(move || try_add_firewall_rule(port));
        self.server.running = true;
        self.server_stop = Some(stop_for_self);
        self.server_status_rx = Some(st_rx);
        self.push_log(format!("server starting on port {port} ..."));
    }

    fn start_transfer(&mut self, dir: Direction) {
        if !self.local_file_str.is_empty() {
            self.local_file = PathBuf::from(self.local_file_str.clone());
        }
        // GET 模式容错：若 local_file_str 指向已存在的目录，自动拼接 Remote 文件名。
        if matches!(dir, Direction::Get) && self.local_file.is_dir() && !self.remote_file.is_empty() {
            self.local_file = self.local_file.join(&self.remote_file);
            self.local_file_str = self.local_file.display().to_string();
        }
        let id = self.next_id;
        self.next_id += 1;
        let target = format!("{}:{}/{}", self.remote_host, self.remote_port, self.remote_file);
        self.transfers.push(TransferState {
            id,
            direction: dir.clone(),
            target,
            bytes: 0,
            blocks: 0,
            total: None,
            started: Instant::now(),
            done: false,
            error: None,
            bps_ema: 0.0,
        });
        let msg_tx: Sender<GuiMsg> = self.msg_tx.clone();
        let host = self.remote_host.clone();
        let port = self.remote_port;
        let file = self.remote_file.clone();
        let local = self.local_file.clone();
        let blksize = self.blksize;
        let window = self.window;
        let ipv6 = self.ipv6;
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = msg_tx.send(GuiMsg::Done {
                        id,
                        bytes: 0,
                        error: Some(format!("rt: {e}")),
                    });
                    return;
                }
            };
            rt.block_on(async move {
                let cfg = last_tftp_core::session::ClientConfig {
                    blksize,
                    timeout_secs: 5,
                    windowsize: window,
                    retries: 4,
                    resume_from_block: 0,
                };
                let client = Client::new(cfg).prefer_ipv6(ipv6);
                let target_str = format!("{host}:{file}");
                let cb_tx = msg_tx.clone();
                let client = client.with_progress(move |p: Progress| {
                    let _ = cb_tx.send(GuiMsg::Progress {
                        id,
                        bytes: p.bytes,
                        blocks: p.blocks,
                        total: p.total_bytes,
                    });
                });
                let result = match dir {
                    Direction::Get => client.get(&target_str, port, &local).await,
                    Direction::Put => client.put(&local, &target_str, port).await,
                };
                let final_bytes = result.as_ref().map(|s| s.bytes).unwrap_or(0);
                let _ = msg_tx.send(GuiMsg::Done {
                    id,
                    bytes: final_bytes,
                    error: result.err().map(|e| e.to_string()),
                });
            });
        });
    }

    fn poll_messages(&mut self) {
        let mut server_lines: Vec<String> = Vec::new();
        if let Some(rx) = self.server_status_rx.as_ref() {
            while let Ok(line) = rx.try_recv() {
                server_lines.push(line);
            }
        }
        for line in server_lines {
            self.push_log(line);
        }
        // 服务端传输事件：先 drain 出来（避免 push_log 持 &self 时的借用冲突）。
        let mut server_events: Vec<ServerTransferEvent> = Vec::new();
        if let Some(rx) = self.server_transfer_rx.as_ref() {
            while let Ok(ev) = rx.try_recv() { server_events.push(ev); }
        }
        let mut server_log_lines: Vec<String> = Vec::new();
        for ev in server_events {
            let dir_str = |d: &Direction| match d { Direction::Get => "GET", Direction::Put => "PUT" };
            if !ev.done {
                let target = format!("server://{}/{}", ev.peer, ev.filename);
                server_log_lines.push(format!("[server] {} from {} file={}", dir_str(&ev.direction), ev.peer, ev.filename));
                self.transfers.push(TransferState {
                    id: self.next_id, direction: ev.direction, target,
                    bytes: 0, blocks: 0, total: None, started: Instant::now(),
                    done: false, error: None, bps_ema: 0.0,
                });
                self.next_id += 1;
            } else if let Some(t) = self.transfers.iter_mut().rev().find(|t| {
                t.target.starts_with("server://") && t.target.contains(&ev.peer)
                    && t.target.ends_with(&ev.filename) && !t.done
            }) {
                t.done = true;
                t.error = ev.error.clone();
                if ev.bytes > t.bytes { t.bytes = ev.bytes; }
                if ev.total.is_some() { t.total = ev.total; }
                let log_line = if let Some(ref err) = ev.error {
                    format!("[server] {} {} FAILED: {} ({} bytes)", dir_str(&t.direction), t.target, err, t.bytes)
                } else {
                    format!("[server] {} {} DONE ({} bytes)", dir_str(&t.direction), t.target, t.bytes)
                };
                server_log_lines.push(log_line);
            }
        }
        for line in server_log_lines { self.push_log(line); }

        // 服务端传输进度：每来一个 progress 事件更新匹配 transfer 的 bytes + total。
        if let Some(rx) = self.server_progress_rx.as_ref() {
            while let Ok((bytes, total)) = rx.try_recv() {
                if let Some(t) = self.transfers.iter_mut().rev().find(|t| {
                    t.target.starts_with("server://") && !t.done
                }) {
                    let dt = t.started.elapsed().as_secs_f64().max(0.001);
                    let inst = bytes as f64 / dt;
                    t.bps_ema = if t.bps_ema == 0.0 { inst } else { t.bps_ema * 0.7 + inst * 0.3 };
                    t.bytes = bytes;
                    t.total = total;
                }
            }
        }
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                GuiMsg::Progress { id, bytes, blocks, total } => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                        let dt = t.started.elapsed().as_secs_f64().max(0.001);
                        let inst = bytes as f64 / dt;
                        t.bps_ema = if t.bps_ema == 0.0 { inst } else { t.bps_ema * 0.7 + inst * 0.3 };
                        t.bytes = bytes;
                        t.blocks = blocks;
                        t.total = total;
                    }
                }
                GuiMsg::Done { id, bytes, error } => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                        t.done = true;
                        t.error = error;
                        if t.bytes == 0 || bytes > t.bytes {
                            t.bytes = bytes;
                        }
                    }
                }
            }
        }
    }

    // === 样式常量 ===

    const C_BG: egui::Color32 = egui::Color32::from_rgb(28, 31, 37);
    const C_PANEL: egui::Color32 = egui::Color32::from_rgb(20, 23, 28);
    const C_TOPBAR: egui::Color32 = egui::Color32::from_rgb(24, 27, 33);
    const C_CARD: egui::Color32 = egui::Color32::from_rgb(35, 39, 46);
    const C_BORDER: egui::Color32 = egui::Color32::from_rgb(60, 64, 72);
    const C_TEXT: egui::Color32 = egui::Color32::from_rgb(230, 230, 230);
    const C_TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(180, 180, 180);
    const C_TEXT_FAINT: egui::Color32 = egui::Color32::from_rgb(140, 140, 140);
    const C_ACCENT: egui::Color32 = egui::Color32::from_rgb(120, 200, 255);
    const C_SUCCESS: egui::Color32 = egui::Color32::from_rgb(80, 200, 120);
    const C_DANGER: egui::Color32 = egui::Color32::from_rgb(220, 100, 100);
    #[allow(dead_code)]
    const C_WARNING: egui::Color32 = egui::Color32::from_rgb(220, 160, 80);
    const C_BTN_PRIMARY: egui::Color32 = egui::Color32::from_rgb(60, 140, 220);
    const C_BTN_UPLOAD: egui::Color32 = egui::Color32::from_rgb(200, 140, 60);
    const C_BTN_DANGER: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);
    const C_BTN_SUCCESS: egui::Color32 = egui::Color32::from_rgb(80, 180, 120);
    const C_BTN_GHOST: egui::Color32 = egui::Color32::from_rgb(55, 59, 66);
    const C_ROW_BG: egui::Color32 = egui::Color32::from_rgb(28, 32, 38);

    fn nav_item(ui: &mut egui::Ui, tab: &mut Tab, target: Tab, label: &str) {
        let is_active = *tab == target;
        let text = egui::RichText::new(label)
            .size(15.0)
            .strong()
            .color(if is_active {
                egui::Color32::WHITE
            } else {
                Self::C_TEXT_DIM
            });
        let response = ui.add_sized(
            [180.0, 36.0],
            egui::Label::new(text)
                .sense(egui::Sense::click())
                .selectable(false),
        );
        if response.clicked() {
            *tab = target;
        }
        if is_active {
            let rect = response.rect;
            let painter = ui.painter_at(rect);
            painter.rect_filled(
                rect,
                0.0,
                egui::Color32::from_rgba_premultiplied(120, 200, 255, 30),
            );
            painter.line_segment(
                [
                    rect.left_center() - egui::vec2(0.0, 12.0),
                    rect.left_center() + egui::vec2(0.0, 12.0),
                ],
                egui::Stroke::new(3.0_f32, Self::C_ACCENT),
            );
        }
    }

    // === 渲染各 tab ===

    fn draw_server_tab(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style())
            .fill(Self::C_CARD)
            .stroke(egui::Stroke::new(1.0, Self::C_BORDER))
            .rounding(egui::Rounding::same(10))
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("📂  TFTP Server")
                            .strong()
                            .size(15.0)
                            .color(Self::C_TEXT),
                    );
                    ui.add_space(14.0);

                    ui.label(
                        egui::RichText::new("Serve directory")
                            .size(12.0)
                            .color(Self::C_TEXT_FAINT),
                    );
                    ui.add_space(4.0);
                    let avail = ui.available_width();
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [avail - 100.0, 30.0],
                            egui::TextEdit::singleline(&mut self.server_root_str)
                                .hint_text("/path/to/share")
                                .desired_width(avail - 100.0),
                        );
                        if Self::ghost_button(ui, "Browse…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_directory(&self.server_root_str)
                                .pick_folder()
                            {
                                self.server_root_str = dir.display().to_string();
                            }
                        }
                    });

                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new("Listening port")
                            .size(12.0)
                            .color(Self::C_TEXT_FAINT),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [140.0, 30.0],
                            egui::DragValue::new(&mut self.server.port).range(1..=65535u16),
                        );
                        ui.add_space(20.0);
                        ui.checkbox(
                            &mut self.server.allow_write,
                            egui::RichText::new("Allow write (PUT)")
                                .size(13.0)
                                .color(Self::C_TEXT),
                        );
                    });

                    ui.add_space(20.0);
                    egui::Frame::group(ui.style())
                        .fill(Self::C_PANEL)
                        .stroke(egui::Stroke::new(1.0, Self::C_BORDER))
                        .rounding(egui::Rounding::same(8))
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Access endpoints")
                                        .size(11.0)
                                        .color(Self::C_TEXT_FAINT),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(format!("127.0.0.1:{}", self.server.port))
                                        .monospace()
                                        .size(13.0)
                                        .color(Self::C_TEXT),
                                );
                                if self.local_ips.is_empty() {
                                    ui.label(
                                        egui::RichText::new("(no LAN IP detected)")
                                            .size(11.0)
                                            .color(Self::C_TEXT_FAINT),
                                    );
                                } else {
                                    for ip in &self.local_ips {
                                        ui.label(
                                            egui::RichText::new(format!("{}:{}", ip, self.server.port))
                                                .monospace()
                                                .size(13.0)
                                                .color(Self::C_TEXT),
                                        );
                                    }
                                }
                            });
                        });

                    ui.add_space(28.0);
                    let (accent, label) = if self.server.running {
                        (Self::C_BTN_DANGER, "■  Stop server")
                    } else {
                        (Self::C_BTN_SUCCESS, "▶  Start server")
                    };
                    if Self::primary_button(ui, label, accent).clicked() {
                        if self.server.running {
                            if let Some(stop) = self.server_stop.take() {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            self.server.running = false;
                            self.push_log("server stop requested".to_string());
                        } else {
                            self.start_server();
                        }
                    }
                });
            });
    }

    fn draw_client_tab(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style())
            .fill(Self::C_CARD)
            .stroke(egui::Stroke::new(1.0, Self::C_BORDER))
            .rounding(egui::Rounding::same(10))
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("⇣  Get file")
                                .strong()
                                .size(15.0)
                                .color(Self::C_TEXT),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new("pull a file from a remote TFTP server")
                                        .size(11.0)
                                        .color(Self::C_TEXT_FAINT),
                                );
                            },
                        );
                    });
                    ui.add_space(14.0);

                    ui.horizontal(|ui| {
                        Self::field_label(ui, "Host");
                        ui.add_sized(
                            [260.0, 28.0],
                            egui::TextEdit::singleline(&mut self.remote_host)
                                .hint_text("192.168.1.1")
                                .desired_width(260.0),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        Self::field_label(ui, "Port");
                        ui.add_sized(
                            [90.0, 28.0],
                            egui::DragValue::new(&mut self.remote_port).range(1..=65535u16),
                        );
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new("Block")
                                .color(Self::C_TEXT_DIM)
                                .size(13.0),
                        );
                        ui.add_sized(
                            [90.0, 28.0],
                            egui::DragValue::new(&mut self.blksize).range(8..=65464u16),
                        );
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new("Window")
                                .color(Self::C_TEXT_DIM)
                                .size(13.0),
                        );
                        ui.add_sized(
                            [60.0, 28.0],
                            egui::DragValue::new(&mut self.window).range(1..=64u16),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        Self::field_label(ui, "Remote");
                        ui.add_sized(
                            [260.0, 28.0],
                            egui::TextEdit::singleline(&mut self.remote_file)
                                .hint_text("start.sh")
                                .desired_width(260.0),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        Self::field_label(ui, "Local");
                        let remaining = (ui.available_width() - 90.0).max(120.0);
                        ui.allocate_ui(egui::vec2(remaining, 28.0), |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.local_file_str)
                                    .hint_text("/path/to/save"),
                            );
                        });
                        if Self::ghost_button(ui, "Browse…").clicked() {
                            self.pick_local_path();
                        }
                    });

                    ui.add_space(20.0);
                    let enabled = !self.remote_file.is_empty() && !self.local_file_str.is_empty();
                    let dl_fill = if enabled {
                        Self::C_BTN_PRIMARY
                    } else {
                        egui::Color32::from_rgb(80, 80, 90)
                    };
                    let up_fill = if enabled {
                        Self::C_BTN_UPLOAD
                    } else {
                        egui::Color32::from_rgb(80, 80, 90)
                    };
                    ui.horizontal(|ui| {
                        if Self::primary_button(ui, "⇣  Download", dl_fill).clicked() && enabled {
                            self.last_action = LastAction::Get;
                            self.start_transfer(Direction::Get);
                        }
                        ui.add_space(8.0);
                        if Self::primary_button(ui, "⇡  Upload", up_fill).clicked() && enabled {
                            self.last_action = LastAction::Put;
                            self.start_transfer(Direction::Put);
                        }
                    });
                });
            });
    }

    fn draw_transfers_tab(&mut self, ui: &mut egui::Ui) {
        let active = self.transfers.iter().filter(|t| !t.done).count();
        let total = self.transfers.len();
        let done = total - active;
        let subtitle = format!("{active} in flight  ·  {done} done  ·  {total} total");

        egui::Frame::group(ui.style())
            .fill(Self::C_CARD)
            .stroke(egui::Stroke::new(1.0, Self::C_BORDER))
            .rounding(egui::Rounding::same(10))
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("⟳  Transfers")
                                .strong()
                                .size(15.0)
                                .color(Self::C_TEXT),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new(&subtitle)
                                        .size(11.0)
                                        .color(Self::C_TEXT_FAINT),
                                );
                            },
                        );
                    });
                    ui.add_space(12.0);

                    if self.transfers.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(
                                egui::RichText::new("No transfers yet")
                                    .color(Self::C_TEXT_FAINT)
                                    .size(15.0),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Start one from the Client tab")
                                    .color(egui::Color32::from_rgb(90, 90, 90))
                                    .size(12.0),
                            );
                        });
                        return;
                    }

                    // 复制 transfers 用于渲染（避免 borrow self.transfers 冲突）
                    let snapshot: Vec<TransferState> = self.transfers.clone();
                    let mut remove_idx: Option<usize> = None;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, t) in snapshot.iter().enumerate() {
                                ui.add_space(4.0);
                                Self::draw_transfer_row(ui, t);
                                ui.add_space(8.0);
                                if t.done && Self::ghost_button(ui, "remove").clicked() {
                                    remove_idx = Some(i);
                                }
                            }
                        });
                    if let Some(i) = remove_idx {
                        self.transfers.remove(i);
                    }
                });
            });
    }

    fn draw_transfer_row(ui: &mut egui::Ui, t: &TransferState) {
        let dir_str = match t.direction {
            Direction::Get => "⇣ GET",
            Direction::Put => "⇡ PUT",
        };
        // 状态颜色 + 标签
        let (status_color, status_label) = if let Some(err) = &t.error {
            (Self::C_DANGER, format!("FAILED: {err}"))
        } else if t.done {
            (Self::C_SUCCESS, "DONE".into())
        } else {
            (Self::C_ACCENT, format!("{:.1} KB/s", t.bps_ema / 1000.0))
        };
        // 从 target 抽文件名（target 形如 "host:port/file"）
        let filename = t.target.rsplit('/').next().unwrap_or(&t.target).to_string();
        // 进度：始终用 bytes/total 计算百分比（缓慢增长）。
        // 没有 total 时（服务端 transfer）：用已传 bytes 的 log 估算，
        // 让进度条缓慢前行但不依赖传输速度。
        let (progress, pct) = if let Some(total) = t.total {
            if total > 0 {
                let p = (t.bytes as f32 / total as f32).clamp(0.0, 1.0);
                (p, format!("{:.1}%", p * 100.0))
            } else {
                (0.0, "—".into())
            }
        } else if t.bytes > 0 && !t.done {
            // 没有 total 的场景（server side）：按已传 bytes 做 log 估算，
            // 让进度条单调递增且缓慢前行，不随速度跳动。
            let log_est = (t.bytes as f64 + 1.0).ln() as f32;
            let p = (log_est / 22.0).clamp(0.02, 0.98);
            (p, format!("{} / ? ({:.0} KB/s)", human_bytes(t.bytes), t.bps_ema / 1000.0))
        } else if t.bytes > 0 && t.done {
            (1.0, format!("{} (DONE)", human_bytes(t.bytes)))
        } else {
            (0.0, "…".into())
        };
        // 大小文本
        let size_text = match t.total {
            Some(total) => format!("{} / {} ({})", human_bytes(t.bytes), human_bytes(total), pct),
            None => format!("{} ({})", human_bytes(t.bytes), pct),
        };

        egui::Frame::group(ui.style())
            .fill(Self::C_ROW_BG)
            .rounding(egui::Rounding::same(6))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(dir_str)
                                .strong()
                                .size(12.0)
                                .color(status_color),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(&filename)
                                .strong()
                                .size(14.0)
                                .color(Self::C_TEXT),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .fill(status_color)
                                .desired_width(420.0)
                                
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(&size_text)
                                .size(11.0)
                                .color(Self::C_TEXT_DIM),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(&status_label)
                                .size(11.0)
                                .color(status_color),
                        );
                    });
                });
            });
    }

    fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style())
            .fill(Self::C_CARD)
            .stroke(egui::Stroke::new(1.0, Self::C_BORDER))
            .rounding(egui::Rounding::same(10))
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("⚙  Settings")
                            .strong()
                            .size(15.0)
                            .color(Self::C_TEXT),
                    );
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("Networking")
                            .size(12.0)
                            .color(Self::C_TEXT_FAINT),
                    );
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.ipv6,
                        egui::RichText::new("Prefer IPv6 dual-stack")
                            .size(13.0)
                            .color(Self::C_TEXT),
                    );
                    ui.add_space(24.0);
                    ui.label(
                        egui::RichText::new("About")
                            .size(12.0)
                            .color(Self::C_TEXT_FAINT),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("last-tftp v0.1.0  ·  Modern TFTP client & server")
                            .color(Self::C_TEXT_DIM)
                            .size(12.0),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("RFC 1350 + 2347/2348/2349/7440  ·  IPv4 + IPv6")
                            .color(Self::C_TEXT_FAINT)
                            .size(11.0),
                    );
                });
            });
    }

    fn draw_topbar(&self, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.heading(
                egui::RichText::new("⬢ last-tftp")
                    .strong()
                    .size(18.0)
                    .color(Self::C_ACCENT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, dot, label) = if self.server.running {
                    (Self::C_SUCCESS, "●", format!("server:{}", self.server.port))
                } else {
                    (Self::C_TEXT_DIM, "○", "server: off".into())
                };
                ui.label(
                    egui::RichText::new(format!("{dot} {label}"))
                        .color(color)
                        .size(13.0),
                );
                ui.separator();
                let mut ipv6 = self.ipv6;
                let cb = ui.checkbox(&mut ipv6, "IPv6");
                if cb.changed() {
                    // 简单做法：通过 push_log 提示下次刷新
                    drop(cb);
                }
            });
        });
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new("NAVIGATION")
                        .size(11.0)
                        .color(Self::C_TEXT_FAINT)
                        .strong(),
                );
            });
            ui.add_space(10.0);
            Self::nav_item(ui, &mut self.tab, Tab::Server, "📂  Server");
            Self::nav_item(ui, &mut self.tab, Tab::Client, "⇣⇡  Client");
            Self::nav_item(ui, &mut self.tab, Tab::Transfers, "⟳  Transfers");
            Self::nav_item(ui, &mut self.tab, Tab::Settings, "⚙  Settings");
        });
    }

    fn draw_statusbar(&self, ui: &mut egui::Ui) {
        let active = self.transfers.iter().find(|t| !t.done);
        let (text, color) = match active {
            Some(t) => {
                let kb = t.bps_ema / 1000.0;
                (
                    format!(
                        "⟳  {}  ·  {} / {} bytes  ·  {:.1} KB/s",
                        t.target,
                        t.bytes,
                        t.total.unwrap_or(0),
                        kb
                    ),
                    Self::C_ACCENT,
                )
            }
            None => {
                if self.server.running {
                    ("✓  server running  ·  ready".into(), Self::C_SUCCESS)
                } else {
                    ("idle".into(), Self::C_TEXT_FAINT)
                }
            }
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&text).color(color).size(11.0));
        });
    }

    // === 辅助 ===

    fn ghost_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
        let btn = egui::Button::new(
            egui::RichText::new(label)
                .size(13.0)
                .color(egui::Color32::from_rgb(200, 200, 200)),
        )
        .fill(Self::C_BTN_GHOST)
        .rounding(egui::Rounding::same(6))
        .min_size(egui::vec2(80.0, 28.0));
        ui.add(btn)
    }

    fn primary_button(ui: &mut egui::Ui, label: &str, fill: egui::Color32) -> egui::Response {
        let btn = egui::Button::new(
            egui::RichText::new(label)
                .strong()
                .size(14.0)
                .color(egui::Color32::WHITE),
        )
        .fill(fill)
        .rounding(egui::Rounding::same(6))
        .min_size(egui::vec2(120.0, 34.0));
        ui.add(btn)
    }

    fn field_label(ui: &mut egui::Ui, label: &str) {
        ui.add_sized(
            [90.0, 22.0],
            egui::Label::new(
                egui::RichText::new(label)
                    .color(Self::C_TEXT_DIM)
                    .size(13.0),
            ),
        );
    }

    pub fn update_inner(&mut self, ctx: &egui::Context) {
        self.poll_messages();

        if let Some((host, port, file, local)) = self.auto_get.take() {
            self.remote_host = host;
            self.remote_port = port;
            self.remote_file = file;
            self.local_file_str = local.display().to_string();
            self.local_file = local;
            self.start_transfer(Direction::Get);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            self.screenshot_requested
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        egui::TopBottomPanel::top("appbar")
            .frame(
                egui::Frame::none()
                    .fill(Self::C_TOPBAR)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                self.draw_topbar(ui);
            });

        egui::TopBottomPanel::bottom("statusbar")
            .frame(
                egui::Frame::none()
                    .fill(Self::C_TOPBAR)
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(ctx, |ui| {
                self.draw_statusbar(ui);
            });

        let tab = self.tab;
        egui::SidePanel::left("sidebar")
            .exact_width(180.0)
            .frame(
                egui::Frame::none()
                    .fill(Self::C_PANEL)
                    .inner_margin(egui::Margin::same(0)),
            )
            .show(ctx, |ui| {
                self.draw_sidebar(ui);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Self::C_BG)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match tab {
                        Tab::Server => self.draw_server_tab(ui),
                        Tab::Client => self.draw_client_tab(ui),
                        Tab::Transfers => self.draw_transfers_tab(ui),
                        Tab::Settings => self.draw_settings_tab(ui),
                    });
            });

        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn do_gl_screenshot(&self) {
        use std::io::Write;
        let Some(gl) = self.gl.clone() else {
            return;
        };
        let (w, h) = self.viewport_size;
        let mut pixels: Vec<u8> = vec![0; (w * h * 4) as usize];
        unsafe {
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA as u32,
                glow::UNSIGNED_BYTE as u32,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
        }
        let out_path = "/tmp/gui_capture.ppm";
        if let Ok(mut file) = std::fs::File::create(out_path) {
            let _ = file.write_all(format!("P6\n{w} {h}\n255\n").as_bytes());
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for chunk in pixels.chunks(4) {
                rgb.push(chunk[0]);
                rgb.push(chunk[1]);
                rgb.push(chunk[2]);
            }
            let _ = file.write_all(&rgb);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.screenshot_requested.load(std::sync::atomic::Ordering::Relaxed) {
            self.screenshot_requested
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.do_gl_screenshot();
        }
        self.update_inner(ctx);
    }
}
/// 收集本机所有接口的 IP（排除 loopback）。
fn collect_local_ips() -> Vec<std::net::IpAddr> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            out.push(iface.ip());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 把字节数格式化为 B / KB / MB / GB。
fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.2} {}", v, UNITS[i])
    }
}

/// 服务端传输事件，server 线程 → GUI 线程。
#[derive(Debug, Clone)]
pub struct ServerTransferEvent {
    pub direction: Direction,
    pub peer: String,
    pub filename: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
}

/// 自己跑的 server 主循环：bind 后每次 RRQ/WRQ 触发 transfer 事件传到 UI。
/// 等价于 core::server::serve，但多一份 transfer 事件回调。
async fn run_server_with_observer(
    bind_addr: std::net::SocketAddr,
    cfg: last_tftp_core::server::ServerConfig,
    tr_tx: std::sync::mpsc::Sender<ServerTransferEvent>,
    pr_tx: std::sync::mpsc::Sender<(u64, Option<u64>)>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::io::Result<()> {
    use last_tftp_core::packet::Packet;
    use last_tftp_core::server::handle_one;
    use std::sync::atomic::Ordering;
    let sock = std::sync::Arc::new(tokio::net::UdpSocket::bind(bind_addr).await?);
    let mut buf = vec![0u8; 65536];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (n, peer) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let pkt = match Packet::parse(&buf[..n]) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let (direction, filename) = match &pkt {
            Packet::Rrq { filename, .. } => (Direction::Get, filename.clone()),
            Packet::Wrq { filename, .. } => (Direction::Put, filename.clone()),
            _ => continue,
        };
        let _ = tr_tx.send(ServerTransferEvent {
            direction: direction.clone(),
            peer: peer.to_string(),
            filename: filename.clone(),
            bytes: 0,
            total: None,
            done: false,
            error: None,
        });
        let cfg2 = cfg.clone();
        let tr_tx2 = tr_tx.clone();
        let pr_tx2 = pr_tx.clone();
        let peer_s = peer.to_string();
        let filename_s = filename.clone();
        let direction_s = direction.clone();
        tokio::spawn(async move {
            let result = handle_one(peer, pkt, cfg2, Some(pr_tx2)).await;
            let (bytes, total, err) = match result {
                Ok(stats) => (stats.bytes, stats.total_bytes, None),
                Err(e) => (0, None, Some(e.to_string())),
            };
            let _ = tr_tx2.send(ServerTransferEvent {
                direction: direction_s, peer: peer_s, filename: filename_s,
                bytes, total, done: true, error: err,
            });
        });
    }
}

/// 启动时尝试给 Windows 防火墙添加 UDP 入站规则（best-effort）。
/// tftpd64 走的是安装时一次性注册；我们做运行时添加（需要管理员权限）。
#[cfg(target_os = "windows")]
fn try_add_firewall_rule(port: u16) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let rule_name = format!("last-tftp UDP {port}");
    let _ = Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule", &format!("name={rule_name}")])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let res = Command::new("netsh")
        .args(["advfirewall", "firewall", "add", "rule", &format!("name={rule_name}"),
               "dir=in", "action=allow", "protocol=UDP",
               &format!("localport={port}"), &format!("program={}", exe.display())])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    if let Ok(out) = res {
        if !out.status.success() {
            eprintln!(
                "[firewall] add rule failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn try_add_firewall_rule(_port: u16) {}

/// 用户最近点的操作按钮（控制 Browse 弹的是文件还是目录对话框）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LastAction {
    #[default]
    Get,
    Put,
}

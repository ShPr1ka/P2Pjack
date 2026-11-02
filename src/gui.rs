//! Окно P2PJack: питание, свой IP, сети и участники, приглашения, трей.
//!
//! Внешний вид — чёрная приборная панель: прямые углы, тонкие линии, Bahnschrift для подписей,
//! Consolas для адресов и один акцентный цвет.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Painter, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2, pos2, vec2,
};
use p2pjack::config::Settings;
use p2pjack::platform;
use p2pjack::ui_kit::*;
use p2pjack::service::{self, Cmd, MemberState, MemberView, NetworkView, Power, ServiceHandle, Snapshot};
use std::collections::{HashSet, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------- запуск ----------

pub struct Options {
    pub demo: bool,
    pub no_elevate: bool,
    /// Запуск из автозагрузки Windows.
    pub autostart: bool,
    /// Не показывать окно при старте — сразу в трей.
    pub start_hidden: bool,
    pub screenshot: Option<PathBuf>,
    pub demo_dialog: Option<String>,
    /// Остановить заставку на этом моменте (мс) — для скриншотов её кадров.
    pub splash_at: Option<u32>,
    /// Проверка трея: окно прячется, вторая «копия» его возвращает, делается снимок.
    pub selftest_tray: bool,
}

impl Options {
    pub fn from_args() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
        Self {
            demo: args.iter().any(|a| a == "--demo"),
            no_elevate: args.iter().any(|a| a == "--no-elevate"),
            autostart: args.iter().any(|a| a == "--autostart"),
            start_hidden: false,
            screenshot: value("--screenshot").map(PathBuf::from),
            demo_dialog: value("--demo-dialog"),
            splash_at: value("--splash-at").and_then(|v| v.parse().ok()),
            selftest_tray: args.iter().any(|a| a == "--selftest-tray"),
        }
    }
}

/// Если программа уже запущена — просит её показать окно и возвращает `true`.
pub fn signal_running_instance() -> bool {
    platform::signal_instance("show")
}

/// Журнал событий для панели «Журнал».
#[derive(Clone, Default)]
pub struct Logs(Arc<Mutex<VecDeque<String>>>);

impl Logs {
    pub fn make_writer(&self) -> impl Fn() -> LogWriter + Send + Sync + 'static {
        let sink = self.0.clone();
        move || LogWriter { buf: Vec::new(), sink: sink.clone() }
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().cloned().collect()
    }
}

pub struct LogWriter {
    buf: Vec<u8>,
    sink: Arc<Mutex<VecDeque<String>>>,
}

impl std::io::Write for LogWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.buf);
        let mut q = self.sink.lock().unwrap();
        q.extend(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
        while q.len() > 500 {
            q.pop_front();
        }
    }
}

pub fn run(opts: Options, logs: Logs) -> eframe::Result {
    let icon = mark_icon(64, IconState::On);
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("P2PJack")
            .with_inner_size([460.0, 700.0])
            .with_min_inner_size([400.0, 520.0])
            .with_visible(!opts.start_hidden)
            // Рамку и кнопки окна рисуем сами, в стиле приложения.
            .with_decorations(false)
            .with_icon(Arc::new(egui::IconData { rgba: icon, width: 64, height: 64 })),
        ..Default::default()
    };
    eframe::run_native(
        "P2PJack",
        native,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            Ok(Box::new(App::new(&cc.egui_ctx, opts, logs)?))
        }),
    )
}

// ---------- приложение ----------

enum Dialog {
    Create { name: String, password: String, confirm: String, error: Option<String> },
    Join { name: String, password: String, error: Option<String> },
    Invite { network: String },
    Leave { network: String },
    Connect { network: String, addr: String, error: Option<String> },
    /// `autostart` — есть ли задание автозапуска в системе (оно живёт вне настроек программы).
    Settings { nick: String, settings: Settings, autostart: bool },
}

/// Что сделать после показа диалога (диалог вынут из `App` на время отрисовки).
#[derive(Default)]
struct DialogOut {
    close: bool,
    next: Option<Dialog>,
    cmds: Vec<Cmd>,
    toast: Option<String>,
    /// Включить или выключить автозапуск с Windows.
    autostart: Option<bool>,
}

/// Команды окну из трея и от второй копии программы.
enum UiCmd {
    Show,
    TogglePower,
    Quit,
}

struct Shot {
    path: PathBuf,
    frame: u32,
    done: Arc<AtomicBool>,
}

pub struct App {
    service: Option<ServiceHandle>,
    demo: Option<Snapshot>,
    logs: Logs,
    dialog: Option<Dialog>,
    collapsed: HashSet<String>,
    show_log: bool,
    toast: Option<(String, Instant)>,
    elevated: bool,
    ui_rx: mpsc::Receiver<UiCmd>,
    tray: Option<Tray>,
    hidden: bool,
    /// Сколько первых кадров прятать окно: eframe сам показывает его после первой отрисовки.
    hide_on_start: u8,
    quitting: bool,
    frame: u64,
    shot: Option<Shot>,
    selftest: Option<u64>,
    splash: Option<crate::splash::Splash>,
    _rt: Option<tokio::runtime::Runtime>,
}

impl App {
    fn new(ctx: &egui::Context, opts: Options, logs: Logs) -> anyhow::Result<Self> {
        let (service, demo, rt) = if opts.demo {
            (None, Some(demo_snapshot()), None)
        } else {
            let rt = tokio::runtime::Runtime::new()?;
            let repaint = ctx.clone();
            let svc = service::start(rt.handle(), move || repaint.request_repaint())?;
            (Some(svc), None, Some(rt))
        };
        let (ui_tx, ui_rx) = mpsc::channel();
        listen_for_instances(ui_tx.clone(), ctx.clone());
        let tray = match Tray::new(ui_tx.clone(), ctx.clone()) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("Значок в трее не создан: {e:#}");
                None
            }
        };
        let dialog = opts.demo_dialog.as_deref().and_then(|d| demo_dialog(d, demo.as_ref()?));
        // Заставки нет при запуске в трей и на скриншотах интерфейса (если не просили её кадр).
        let splash = (!opts.start_hidden
            && !opts.selftest_tray
            && (opts.screenshot.is_none() || opts.splash_at.is_some()))
        .then(|| crate::splash::Splash::new(opts.splash_at));
        Ok(Self {
            service,
            demo,
            logs,
            dialog,
            collapsed: HashSet::new(),
            show_log: false,
            toast: None,
            elevated: opts.demo || platform::is_elevated(),
            ui_rx,
            tray,
            hidden: opts.start_hidden,
            hide_on_start: if opts.start_hidden { 3 } else { 0 },
            quitting: false,
            frame: 0,
            shot: opts.screenshot.map(|path| Shot { path, frame: 0, done: Arc::new(AtomicBool::new(false)) }),
            selftest: opts.selftest_tray.then_some(0),
            splash,
            _rt: rt,
        })
    }

    fn snapshot(&self) -> Snapshot {
        match (&self.service, &self.demo) {
            (Some(s), _) => s.snapshot.borrow().clone(),
            (None, Some(d)) => d.clone(),
            (None, None) => unreachable!("either a service or a demo snapshot"),
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(s) = &self.service {
            s.send(cmd);
        }
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn copy(&mut self, ctx: &egui::Context, text: String, what: &str) {
        ctx.copy_text(text);
        self.toast(format!("{what} скопирован"));
    }

    /// Создаёт или удаляет задание автозапуска, если его состояние изменилось.
    fn apply_autostart(&mut self, enable: bool) {
        if self.demo.is_some() || enable == platform::autostart_enabled() {
            return;
        }
        let result = std::env::current_exe()
            .map_err(anyhow::Error::from)
            .and_then(|exe| platform::set_autostart(enable, &exe));
        match result {
            Ok(()) if enable => self.toast("Автозапуск включён"),
            Ok(()) => self.toast("Автозапуск выключен"),
            Err(e) => {
                tracing::warn!("Автозапуск: {e:#}");
                self.toast("Не удалось изменить автозапуск — см. журнал");
            }
        }
    }

    fn toggle_power(&self, snap: &Snapshot) {
        self.send(Cmd::SetPower(matches!(snap.power, Power::Off | Power::Failed(_))));
    }
}

/// Вторая копия программы присылает «show», установщик — «quit» (перед обновлением или удалением).
fn listen_for_instances(tx: mpsc::Sender<UiCmd>, ctx: egui::Context) {
    let Ok(sock) = UdpSocket::bind(("127.0.0.1", platform::INSTANCE_PORT)) else { return };
    std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        while let Ok((n, _)) = sock.recv_from(&mut buf) {
            let cmd = match &buf[..n] {
                b"show" => UiCmd::Show,
                b"quit" => UiCmd::Quit,
                _ => continue,
            };
            if tx.send(cmd).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}

impl eframe::App for App {
    /// Вызывается и когда окно спрятано в трей: здесь — всё, что не рисует.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.hide_on_start > 0 && self.hidden {
            self.hide_on_start -= 1;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.request_repaint();
        }
        let snap = self.snapshot();
        let tray_enabled = self.tray.is_some() && snap.settings.tray;
        if !self.hidden && !self.quitting && tray_enabled && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden = true;
        }
        while let Ok(cmd) = self.ui_rx.try_recv() {
            match cmd {
                UiCmd::Show => {
                    self.hidden = false;
                    self.hide_on_start = 0;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                UiCmd::TogglePower => self.toggle_power(&snap),
                UiCmd::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if let Some(tray) = &mut self.tray {
            tray.update(&snap);
        }
        self.selftest_step(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let snap = self.snapshot();
        self.frame += 1;

        title_bar(ui, true, |ui| {
            if text_button(ui, "Настройки", col::MUTED).clicked() {
                let autostart = self.demo.is_none() && platform::autostart_enabled();
                self.dialog =
                    Some(Dialog::Settings { nick: snap.nick.clone(), settings: snap.settings.clone(), autostart });
            }
        });
        self.header(ui, &snap);
        self.footer(ui, &snap);
        if self.show_log {
            self.log_panel(ui);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(col::BG).inner_margin(Margin::symmetric(16, 12)))
            .show(ui, |ui| self.body(ui, &snap));

        if let Some(mut dialog) = self.dialog.take() {
            let out = show_dialog(&ctx, &mut dialog, &snap);
            for cmd in out.cmds {
                self.send(cmd);
            }
            if let Some(t) = out.toast {
                self.toast(t);
            }
            if let Some(enable) = out.autostart {
                self.apply_autostart(enable);
            }
            self.dialog = match (out.close, out.next) {
                (_, Some(next)) => Some(next),
                (true, None) => None,
                (false, None) => Some(dialog),
            };
        }
        self.paint_toast(&ctx);
        if let Some(splash) = &mut self.splash
            && !splash.show(&ctx, splash_status(&snap))
        {
            self.splash = None;
        }
        window_frame(&ctx, true);
        self.screenshot_step(&ctx);
        let fast = snap.power == Power::Starting || self.toast.is_some();
        ctx.request_repaint_after(Duration::from_millis(if fast { 30 } else { 500 }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(s) = &self.service {
            s.shutdown(Duration::from_secs(3));
        }
    }
}

// ---------- трей ----------

struct Tray {
    icon: tray_icon::TrayIcon,
    power: tray_icon::menu::MenuItem,
    last: Option<(IconState, String, bool)>,
}

impl Tray {
    fn new(tx: mpsc::Sender<UiCmd>, ctx: egui::Context) -> anyhow::Result<Self> {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
        use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

        let open = MenuItem::new("Открыть P2PJack", true, None);
        let power = MenuItem::new("Выключить", true, None);
        let quit = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &power, &PredefinedMenuItem::separator(), &quit])?;

        let ids = (open.id().clone(), power.id().clone(), quit.id().clone());
        let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = if e.id == ids.0 {
                UiCmd::Show
            } else if e.id == ids.1 {
                UiCmd::TogglePower
            } else if e.id == ids.2 {
                UiCmd::Quit
            } else {
                return;
            };
            let _ = menu_tx.send(cmd);
            menu_ctx.request_repaint();
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = tx.send(UiCmd::Show);
                ctx.request_repaint();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("P2PJack")
            .with_icon(tray_image(IconState::Off)?)
            .build()?;
        Ok(Self { icon, power, last: None })
    }

    /// Значок и подсказка в трее отражают состояние.
    fn update(&mut self, snap: &Snapshot) {
        let state = match snap.power {
            Power::On | Power::Starting => IconState::On,
            Power::Off => IconState::Off,
            Power::Failed(_) => IconState::Error,
        };
        let online: usize = snap.networks.iter().map(NetworkView::online).sum();
        let tooltip = match &snap.power {
            Power::On => format!("P2PJack — в сети\n{}  ·  онлайн: {online}", snap.vip),
            Power::Starting => "P2PJack — запуск…".into(),
            Power::Off => "P2PJack — выключен".into(),
            Power::Failed(_) => "P2PJack — ошибка запуска".into(),
        };
        let on = matches!(snap.power, Power::On | Power::Starting);
        let now = (state, tooltip, on);
        if self.last.as_ref() == Some(&now) {
            return;
        }
        if self.last.as_ref().is_none_or(|l| l.0 != state)
            && let Ok(img) = tray_image(state)
        {
            let _ = self.icon.set_icon(Some(img));
        }
        let _ = self.icon.set_tooltip(Some(&now.1));
        self.power.set_text(if on { "Выключить" } else { "Включить" });
        self.last = Some(now);
    }
}

fn tray_image(state: IconState) -> anyhow::Result<tray_icon::Icon> {
    Ok(tray_icon::Icon::from_rgba(mark_icon(32, state), 32, 32)?)
}

// ---------- шапка, подвал, журнал ----------

impl App {
    fn header(&mut self, ui: &mut Ui, snap: &Snapshot) {
        egui::Panel::top("header")
            .exact_size(142.0)
            .frame(Frame::new().fill(col::BG).inner_margin(Margin::symmetric(16, 12)))
            .show(ui, |ui| {
                let full = ui.max_rect().expand2(vec2(16.0, 12.0));
                ui.painter().line_segment([full.left_bottom(), full.right_bottom()], Stroke::new(1.0, col::LINE));

                // Клавиша питания, имя, IP.
                ui.horizontal(|ui| {
                    if power_key(ui, &snap.power).clicked() {
                        self.toggle_power(snap);
                    }
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(&snap.nick).font(display(22.0)).color(col::TEXT));
                        let ip_color = if snap.power == Power::On { col::ACCENT } else { col::TEXT };
                        let ip = ui
                            .add(
                                egui::Label::new(RichText::new(snap.vip.to_string()).font(mono(17.0)).color(ip_color))
                                    .sense(Sense::click()),
                            )
                            .on_hover_text("Ваш адрес в сетях P2PJack — щёлкните, чтобы скопировать");
                        if ip.clicked() {
                            self.copy(ui.ctx(), snap.vip.to_string(), "IP");
                        }
                        let (text, color) = power_caption(snap);
                        ui.label(RichText::new(text).font(mono(11.5)).color(color));
                    });
                });
                ui.add_space(10.0);

                // Действия.
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 8.0) / 2.0;
                    if action_button(ui, "+  Новая сеть", w, true).clicked() {
                        self.dialog = Some(Dialog::Create {
                            name: String::new(),
                            password: String::new(),
                            confirm: String::new(),
                            error: None,
                        });
                    }
                    if action_button(ui, "Войти в сеть", w, false).clicked() {
                        self.dialog = Some(Dialog::Join { name: String::new(), password: String::new(), error: None });
                    }
                });
            });
    }

    fn footer(&mut self, ui: &mut Ui, snap: &Snapshot) {
        egui::Panel::bottom("footer")
            .exact_size(28.0)
            .frame(Frame::new().fill(col::PANEL).inner_margin(Margin::symmetric(16, 0)))
            .show(ui, |ui| {
                let full = ui.max_rect().expand2(vec2(16.0, 0.0));
                ui.painter().line_segment([full.left_top(), full.right_top()], Stroke::new(1.0, col::LINE));
                ui.horizontal_centered(|ui| {
                    let (state, color) = match &snap.power {
                        Power::On => ("В СЕТИ", col::ACCENT),
                        Power::Starting => ("ЗАПУСК", col::AMBER),
                        Power::Off => ("ВЫКЛ", col::DIM),
                        Power::Failed(_) => ("ОШИБКА", col::RED),
                    };
                    let (r, _) = ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::ZERO, color);
                    ui.label(RichText::new(state).font(mono(11.0)).color(color));
                    let online: usize = snap.networks.iter().map(NetworkView::online).sum();
                    let total: usize = snap.networks.iter().map(|n| n.members.len()).sum();
                    ui.label(
                        RichText::new(format!("  сетей {}   онлайн {online}/{total}", snap.networks.len()))
                            .font(mono(11.0))
                            .color(col::MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.show_log { "журнал ▾" } else { "журнал ▴" };
                        let r = ui.add(
                            egui::Label::new(RichText::new(label).font(mono(11.0)).color(col::MUTED)).sense(Sense::click()),
                        );
                        if r.clicked() {
                            self.show_log = !self.show_log;
                        }
                    });
                });
            });
    }

    fn log_panel(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(170.0)
            .size_range(80.0..=400.0)
            .frame(Frame::new().fill(Color32::BLACK).inner_margin(Margin::symmetric(16, 10)))
            .show(ui, |ui| {
                let lines = self.logs.lines();
                ui.horizontal(|ui| {
                    ui.label(caps("Журнал", 11.0, col::DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if text_button(ui, "Скопировать", col::MUTED).clicked() {
                            let text = lines.join("\n");
                            self.copy(ui.ctx(), text, "Журнал");
                        }
                    });
                });
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for line in &lines {
                        let color = if line.contains("WARN") || line.contains("ERROR") { col::AMBER } else { col::MUTED };
                        ui.label(RichText::new(line).font(mono(11.0)).color(color));
                    }
                });
            });
    }
}

// ---------- список сетей ----------

impl App {
    fn body(&mut self, ui: &mut Ui, snap: &Snapshot) {
        if !self.elevated {
            let restart = notice(
                ui,
                col::RED,
                "Нет прав администратора. Без них Windows не даст создать сетевой адаптер.",
                Some("Перезапустить от имени администратора"),
            );
            #[cfg(windows)]
            if restart && platform::relaunch_elevated("") {
                self.quitting = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            #[cfg(not(windows))]
            let _ = restart;
        } else if let Power::Failed(err) = &snap.power {
            if notice(ui, col::RED, &format!("Не удалось включиться: {err}"), Some("Попробовать снова")) {
                self.send(Cmd::SetPower(true));
            }
        } else if snap.power == Power::Off
            && !snap.networks.is_empty()
            && notice(ui, col::DIM, "P2PJack выключен — вы не в сети.", Some("Включить"))
        {
            self.send(Cmd::SetPower(true));
        }
        if snap.vpn_warning {
            notice(
                ui,
                col::AMBER,
                "Включён VPN или прокси в TUN-режиме — напрямую через него не соединиться. \
                 Выключите его или добавьте P2PJack в исключения.",
                None,
            );
        }

        if snap.networks.is_empty() {
            self.empty_state(ui);
            return;
        }

        let online: usize = snap.networks.iter().map(NetworkView::online).sum();
        ui.horizontal(|ui| {
            ui.label(caps("Сети", 11.0, col::DIM));
            ui.label(RichText::new(format!("{:02}", snap.networks.len())).font(mono(11.0)).color(col::MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{online:02}")).font(mono(11.0)).color(col::MUTED));
                ui.label(caps("Онлайн", 11.0, col::DIM));
            });
        });
        ui.add_space(2.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for net in &snap.networks {
                self.network_block(ui, net, snap);
            }
        });
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        ui.add_space(36.0);
        ui.label(
            RichText::new("[ вы ] ──────── ? ──────── [ друг ]")
                .font(mono(13.0))
                .color(col::DIM),
        );
        ui.add_space(18.0);
        ui.label(RichText::new("Нет ни одной сети").font(display(24.0)).color(col::TEXT));
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Создайте сеть и перешлите приглашение друзьям — или вставьте приглашение, которое прислали вам. \
                 Сервер не нужен: компьютеры соединятся напрямую.",
            )
            .color(col::MUTED),
        );
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            if action_button(ui, "+  Новая сеть", 160.0, true).clicked() {
                self.dialog =
                    Some(Dialog::Create { name: String::new(), password: String::new(), confirm: String::new(), error: None });
            }
            if action_button(ui, "Войти в сеть", 160.0, false).clicked() {
                self.dialog = Some(Dialog::Join { name: String::new(), password: String::new(), error: None });
            }
        });
    }

    fn network_block(&mut self, ui: &mut Ui, net: &NetworkView, snap: &Snapshot) {
        let collapsed = self.collapsed.contains(&net.name);
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
        let p = ui.painter();
        p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, col::LINE));
        if resp.hovered() {
            p.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::ZERO, col::PANEL);
        }
        let cy = rect.center().y;
        paint_triangle(p, pos2(rect.left() + 6.0, cy), collapsed);

        let menu_rect = Rect::from_min_size(pos2(rect.right() - 30.0, cy - 13.0), vec2(30.0, 26.0));
        let invite_rect = Rect::from_min_size(pos2(menu_rect.left() - 118.0, cy - 13.0), vec2(112.0, 26.0));
        let online = net.online();
        let total = net.members.len();
        let (count, count_color) = match &snap.power {
            Power::On if online == 0 && net.searching > 0 => {
                let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
                ("поиск".to_string(), if blink { col::AMBER } else { col::DIM })
            }
            Power::On => (format!("{online}/{total}"), if online > 0 { col::ACCENT } else { col::MUTED }),
            _ => (format!("–/{total}"), col::DIM),
        };
        let count_right = invite_rect.left() - 12.0;
        text(p, pos2(count_right, cy), Align2::RIGHT_CENTER, &count, mono(12.5), count_color, 60.0);
        let name_w = count_right - 60.0 - (rect.left() + 22.0);
        text(p, pos2(rect.left() + 22.0, cy), Align2::LEFT_CENTER, &net.name, display(17.0), col::TEXT, name_w);
        if snap.power == Power::On && online == 0 && net.searching > 0 {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }

        // Кнопки поверх строки — в дочерних областях, чтобы не сбивать раскладку строк ниже.
        let mut invite_ui = ui.new_child(UiBuilder::new().max_rect(invite_rect));
        invite_ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        let invite = invite_ui.add_sized(
            invite_rect.size(),
            egui::Button::new(caps("Пригласить", 11.0, col::ACCENT))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::new(1.0, col::ACCENT_DIM)),
        );
        if invite.clicked() {
            self.dialog = Some(Dialog::Invite { network: net.name.clone() });
        }
        {
            let ui = &mut ui.new_child(UiBuilder::new().max_rect(menu_rect));
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.spacing_mut().button_padding = vec2(7.0, 3.0);
            ui.menu_button(RichText::new("···").font(mono(14.0)).color(col::MUTED), |ui| {
                ui.set_min_width(240.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                if menu_item(ui, "Пригласить").clicked() {
                    self.dialog = Some(Dialog::Invite { network: net.name.clone() });
                    ui.close();
                }
                let ext = net.public_addr;
                if ui.add_enabled(ext.is_some(), menu_entry("Скопировать мой внешний адрес")).clicked() {
                    if let Some(a) = ext {
                        self.copy(ui.ctx(), a.to_string(), "Адрес");
                    }
                    ui.close();
                }
                if menu_item(ui, "Подключиться по адресу…").clicked() {
                    self.dialog = Some(Dialog::Connect { network: net.name.clone(), addr: String::new(), error: None });
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new(RichText::new("Покинуть сеть").color(col::RED)).frame(false)).clicked() {
                    self.dialog = Some(Dialog::Leave { network: net.name.clone() });
                    ui.close();
                }
            });
        }
        if resp.clicked() {
            if collapsed {
                self.collapsed.remove(&net.name);
            } else {
                self.collapsed.insert(net.name.clone());
            }
        }

        if !collapsed {
            if net.members.is_empty() {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
                text(
                    ui.painter(),
                    pos2(r.left() + 22.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "никого — отправьте приглашение",
                    mono(12.0),
                    col::DIM,
                    r.width() - 30.0,
                );
            }
            for m in &net.members {
                self.member_row(ui, m);
            }
        }
        ui.add_space(10.0);
    }

    fn member_row(&mut self, ui: &mut Ui, m: &MemberView) {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, CornerRadius::ZERO, col::RAISED);
            p.rect_filled(Rect::from_min_size(rect.min, vec2(2.0, rect.height())), CornerRadius::ZERO, col::ACCENT);
        }
        let cy = rect.center().y;
        let mark = Rect::from_center_size(pos2(rect.left() + 26.0, cy), Vec2::splat(7.0));
        match &m.state {
            MemberState::Direct { .. } => {
                p.rect_filled(mark, CornerRadius::ZERO, col::ACCENT);
            }
            MemberState::Relay { .. } => {
                p.rect_filled(mark, CornerRadius::ZERO, col::AMBER);
            }
            MemberState::Offline => {
                p.rect_stroke(mark, CornerRadius::ZERO, Stroke::new(1.0, col::DIM), StrokeKind::Inside);
            }
        }

        let ip_x = rect.right() - 168.0;
        let name_x = rect.left() + 40.0;
        let offline = m.state == MemberState::Offline;
        let mut job = egui::text::LayoutJob::default();
        job.append(&m.name, 0.0, egui::TextFormat::simple(body(14.0), if offline { col::DIM } else { col::TEXT }));
        if let MemberState::Relay { via } = &m.state {
            job.append(&format!("через {via}"), 8.0, egui::TextFormat::simple(body(12.0), col::AMBER.gamma_multiply(0.8)));
        }
        text_job(p, pos2(name_x, cy), Align2::LEFT_CENTER, job, ip_x - name_x - 10.0);
        let ip_color = if offline { col::DIM } else { col::MUTED };
        text(p, pos2(ip_x, cy), Align2::LEFT_CENTER, &m.vip.to_string(), mono(12.5), ip_color, 110.0);

        let (ping, ping_color) = match &m.state {
            MemberState::Direct { rtt: Some(rtt) } => {
                let ms = rtt.as_millis();
                let c = match ms {
                    0..80 => col::ACCENT,
                    80..200 => col::AMBER,
                    _ => col::RED,
                };
                (format!("{ms} мс"), c)
            }
            MemberState::Direct { rtt: None } => ("…".into(), col::DIM),
            MemberState::Relay { .. } => ("ретр.".into(), col::AMBER.gamma_multiply(0.7)),
            MemberState::Offline => ("—".into(), col::DIM),
        };
        text(p, pos2(rect.right() - 6.0, cy), Align2::RIGHT_CENTER, &ping, mono(12.0), ping_color, 50.0);

        if resp.double_clicked() {
            self.copy(ui.ctx(), m.vip.to_string(), "IP");
        }
        resp.context_menu(|ui| {
            ui.set_min_width(210.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(&m.name).font(display(13.0)).color(col::DIM));
            if menu_item(ui, "Скопировать IP").clicked() {
                self.copy(ui.ctx(), m.vip.to_string(), "IP");
                ui.close();
            }
            if menu_item(ui, "Пинг").clicked() {
                platform::open_ping_window(m.vip);
                ui.close();
            }
            if let Some(addr) = m.addr
                && menu_item(ui, "Скопировать внешний адрес").clicked()
            {
                self.copy(ui.ctx(), addr.to_string(), "Адрес");
                ui.close();
            }
        });
    }

    fn paint_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, since)) = &self.toast else { return };
        let age = since.elapsed().as_secs_f32();
        if age > 2.0 {
            self.toast = None;
            return;
        }
        let alpha = ((2.0 - age) / 0.25).min(1.0);
        egui::Area::new(Id::unique("toast"))
            .anchor(Align2::LEFT_BOTTOM, vec2(16.0, -40.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                let resp = Frame::new()
                    .fill(Color32::BLACK.gamma_multiply(alpha))
                    .stroke(Stroke::new(1.0, col::LINE2.gamma_multiply(alpha)))
                    .inner_margin(Margin { left: 14, right: 12, top: 7, bottom: 7 })
                    .show(ui, |ui| ui.label(RichText::new(msg).font(mono(12.0)).color(col::TEXT.gamma_multiply(alpha))));
                let r = resp.response.rect;
                ui.painter().rect_filled(
                    Rect::from_min_size(r.min, vec2(2.0, r.height())),
                    CornerRadius::ZERO,
                    col::ACCENT.gamma_multiply(alpha),
                );
            });
    }

    /// `--screenshot файл.png`: сохранить окно в картинку и выйти (проверка внешнего вида).
    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        if self.selftest.is_some_and(|s| s < 3) {
            return;
        }
        shot.frame += 1;
        ctx.request_repaint();
        if shot.frame == 10 {
            let (path, done) = (shot.path.clone(), shot.done.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                if let Err(e) = save_png(&path, &img) {
                    eprintln!("скриншот: {e}");
                }
                done.store(true, Ordering::SeqCst);
            })));
        }
        if shot.done.load(Ordering::SeqCst) {
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// `--selftest-tray`: закрыть окно (должно уйти в трей), затем попросить показаться так,
    /// как это делает вторая копия программы, и дать сделать снимок.
    fn selftest_step(&mut self, ctx: &egui::Context) {
        let Some(stage) = self.selftest else { return };
        ctx.request_repaint();
        match stage {
            0 if self.frame > 10 => {
                tracing::info!("selftest: закрываем окно");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                self.selftest = Some(1);
            }
            1 if self.hidden => {
                tracing::info!("selftest: окно спрятано, просим показать");
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(800));
                    // Так же, как вторая копия программы.
                    if !signal_running_instance() {
                        eprintln!("selftest: порт копий никто не слушает");
                    }
                    ctx.request_repaint();
                });
                self.selftest = Some(2);
            }
            2 if !self.hidden => {
                tracing::info!("selftest: окно снова видно");
                self.toast("Окно вернулось из трея");
                self.selftest = Some(3);
            }
            _ => {}
        }
    }
}

// ---------- диалоги ----------

/// Строка состояния на заставке — настоящее состояние запуска.
fn splash_status(snap: &Snapshot) -> (&'static str, Color32) {
    match &snap.power {
        Power::Starting => ("поднимаем адаптер…", col::AMBER),
        Power::On => ("в сети", col::ACCENT),
        Power::Off => ("выключено", col::DIM),
        Power::Failed(_) => ("ошибка запуска", col::RED),
    }
}

fn invite_text(net: &NetworkView, snap: &Snapshot) -> String {
    format!(
        "Заходи ко мне в сеть P2PJack!\nМой IP в сети: {}\nСеть: {}\nПароль: {}",
        snap.vip, net.name, net.password
    )
}

/// Достаёт название и пароль из вставленного приглашения (переводы строк могут пропасть при вставке).
fn parse_invite(s: &str) -> Option<(String, String)> {
    let n = s.find("Сеть:")?;
    let p = s.find("Пароль:")?;
    if p < n {
        return None;
    }
    let name = s[n + "Сеть:".len()..p].trim().to_string();
    let password = s[p + "Пароль:".len()..].lines().next().unwrap_or("").trim().to_string();
    (!name.is_empty() && !password.is_empty()).then_some((name, password))
}

fn show_dialog(ctx: &egui::Context, dialog: &mut Dialog, snap: &Snapshot) -> DialogOut {
    let mut out = DialogOut::default();
    let modal = egui::Modal::new(Id::unique("dialog"))
        .backdrop_color(Color32::from_black_alpha(200))
        .frame(
            Frame::new()
                .fill(col::PANEL)
                .stroke(Stroke::new(1.0, col::LINE2))
                .corner_radius(CornerRadius::same(2))
                .inner_margin(Margin::same(24)),
        )
        .show(ctx, |ui| {
            ui.set_width(350.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            match dialog {
                Dialog::Create { name, password, confirm, error } => {
                    title(ui, "Новая сеть", "Создать сеть", "Название и пароль — это и есть адрес сети. Сервер не нужен.");
                    field(ui, "Название", name, "например, друзья", false);
                    field(ui, "Пароль", password, "от 6 символов", true);
                    field(ui, "Ещё раз пароль", confirm, "", true);
                    error_line(ui, error);
                    let (cancel, ok) = buttons(ui, "Создать");
                    if cancel {
                        out.close = true;
                    } else if ok || enter {
                        let n = name.trim().to_string();
                        *error = validate(&n, password, snap)
                            .or_else(|| (password != confirm).then(|| "Пароли не совпадают".into()));
                        if error.is_none() {
                            out.cmds.push(Cmd::Join { name: n.clone(), password: password.clone() });
                            out.toast = Some(format!("Сеть «{n}» создана"));
                            // Сразу показываем приглашение: осталось отправить его друзьям.
                            out.next = Some(Dialog::Invite { network: n });
                        }
                    }
                }
                Dialog::Join { name, password, error } => {
                    title(ui, "Вход", "Войти в сеть друга", "Вставьте присланное приглашение целиком — название и пароль подставятся сами.");
                    field(ui, "Название или приглашение", name, "например, друзья", false);
                    if let Some((n, p)) = parse_invite(name) {
                        *name = n;
                        *password = p;
                    }
                    field(ui, "Пароль", password, "", true);
                    error_line(ui, error);
                    let (cancel, ok) = buttons(ui, "Войти");
                    if cancel {
                        out.close = true;
                    } else if ok || enter {
                        let n = name.trim().to_string();
                        *error = validate(&n, password, snap);
                        if error.is_none() {
                            out.cmds.push(Cmd::Join { name: n.clone(), password: password.clone() });
                            out.toast = Some(format!("Подключаемся к «{n}»"));
                            out.close = true;
                        }
                    }
                }
                Dialog::Invite { network } => {
                    // Только что созданная сеть может ещё не дойти до снимка службы.
                    let Some(net) = snap.networks.iter().find(|n| &n.name == network).cloned() else {
                        ui.label(RichText::new("готовим приглашение…").font(mono(12.0)).color(col::DIM));
                        ctx.request_repaint();
                        if buttons(ui, "Закрыть").0 {
                            out.close = true;
                        }
                        return;
                    };
                    title(ui, "Приглашение", &net.name, "Перешлите этот текст другу. В P2PJack он нажмёт «Войти в сеть» и вставит его.");
                    let invite = invite_text(&net, snap);
                    Frame::new()
                        .fill(Color32::BLACK)
                        .stroke(Stroke::new(1.0, col::LINE))
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(&invite).font(mono(12.5)).color(col::TEXT));
                        });
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if action_button(ui, "Скопировать", 170.0, true).clicked() {
                            ctx.copy_text(invite.clone());
                            out.toast = Some("Приглашение скопировано".into());
                        }
                        if action_button(ui, "Готово", ui.available_width(), false).clicked() || enter {
                            out.close = true;
                        }
                    });
                }
                Dialog::Leave { network } => {
                    title(ui, "Выход из сети", network, "Сеть пропадёт из списка. Вернуться можно в любой момент, зная название и пароль.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let leave = ui.add(
                                egui::Button::new(caps("Покинуть", 12.5, Color32::BLACK))
                                    .fill(col::RED)
                                    .stroke(Stroke::NONE)
                                    .min_size(vec2(120.0, 34.0)),
                            );
                            if leave.clicked() {
                                out.cmds.push(Cmd::Leave(network.clone()));
                                out.toast = Some(format!("Вы вышли из «{network}»"));
                                out.close = true;
                            }
                            if action_button(ui, "Отмена", 100.0, false).clicked() {
                                out.close = true;
                            }
                        });
                    });
                }
                Dialog::Connect { network, addr, error } => {
                    title(
                        ui,
                        "Вручную",
                        "Подключиться по адресу",
                        "Если друзья не находятся сами: друг копирует свой внешний адрес в меню сети «···» и присылает вам. \
                         Лучше подключиться навстречу друг другу одновременно.",
                    );
                    field(ui, "Внешний адрес друга", addr, "95.24.11.80:47777", false);
                    error_line(ui, error);
                    let (cancel, ok) = buttons(ui, "Подключиться");
                    if cancel {
                        out.close = true;
                    } else if ok || enter {
                        match addr.trim().parse::<SocketAddr>() {
                            Ok(a) => {
                                out.cmds.push(Cmd::Connect { network: network.clone(), addr: a });
                                out.toast = Some(format!("Стучимся к {a}"));
                                out.close = true;
                            }
                            Err(_) => *error = Some("Нужен адрес вида IP:ПОРТ".into()),
                        }
                    }
                }
                Dialog::Settings { nick, settings, autostart } => {
                    title(ui, "Настройки", "Этот компьютер", "");
                    field(ui, "Имя, которое видят другие", nick, "", false);
                    ui.add_space(6.0);
                    ui.label(caps("Запуск", 11.0, col::DIM));
                    ui.checkbox(autostart, "Запускать вместе с Windows");
                    ui.checkbox(&mut settings.tray, "Сворачивать в трей при закрытии");
                    ui.add_space(6.0);
                    ui.label(caps("Подключение", 11.0, col::DIM));
                    ui.checkbox(&mut settings.upnp, "Открывать порт на роутере (UPnP)");
                    ui.checkbox(&mut settings.dht, "Искать друзей через BitTorrent DHT");
                    ui.checkbox(&mut settings.stun, "Узнавать свой внешний адрес (STUN)");
                    ui.checkbox(&mut settings.lan, "Искать в своей локальной сети");
                    ui.add_space(6.0);
                    ui.label(caps("Windows", 11.0, col::DIM));
                    ui.checkbox(&mut settings.firewall, "Пускать входящие подключения от участников");
                    ui.checkbox(&mut settings.mc_lan, "Показывать LAN-миры Minecraft");
                    ui.add_space(4.0);
                    ui.label(RichText::new("Настройки подключения переподключат все сети.").font(mono(11.0)).color(col::DIM));
                    let (cancel, ok) = buttons(ui, "Сохранить");
                    if cancel {
                        out.close = true;
                    } else if ok {
                        let n = nick.trim();
                        if !n.is_empty() && n != snap.nick {
                            out.cmds.push(Cmd::SetNick(n.to_string()));
                        }
                        if *settings != snap.settings {
                            out.cmds.push(Cmd::SetSettings(settings.clone()));
                        }
                        out.autostart = Some(*autostart);
                        out.close = true;
                    }
                }
            }
        });
    if modal.should_close() {
        out.close = true;
    }
    out
}

fn validate(name: &str, password: &str, snap: &Snapshot) -> Option<String> {
    if name.is_empty() {
        Some("Введите название сети".into())
    } else if name.chars().count() > 40 {
        Some("Название длиннее 40 символов".into())
    } else if snap.networks.iter().any(|n| n.name == name) {
        Some("Такая сеть уже есть в списке".into())
    } else if password.chars().count() < 6 {
        Some("Пароль — минимум 6 символов".into())
    } else {
        None
    }
}

/// Заголовок диалога: метка капсом, крупная строка, пояснение.
fn title(ui: &mut Ui, label: &str, text: &str, note: &str) {
    ui.label(caps(label, 11.0, col::ACCENT));
    ui.label(RichText::new(text).font(display(23.0)).color(col::TEXT));
    if !note.is_empty() {
        ui.label(RichText::new(note).size(13.0).color(col::MUTED));
    }
    ui.add_space(10.0);
}

fn field(ui: &mut Ui, label: &str, value: &mut String, hint: &str, password: bool) {
    ui.label(caps(label, 10.5, col::DIM));
    ui.add(
        TextEdit::singleline(value)
            .hint_text(RichText::new(hint).color(col::DIM))
            .password(password)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(10, 8)),
    );
    ui.add_space(4.0);
}

fn error_line(ui: &mut Ui, error: &Option<String>) {
    if let Some(e) = error {
        ui.label(RichText::new(format!("× {e}")).font(mono(12.0)).color(col::RED));
    }
}

/// «Отмена» и основное действие справа. Возвращает (отмена, ок).
fn buttons(ui: &mut Ui, ok_text: &str) -> (bool, bool) {
    ui.add_space(12.0);
    let mut res = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            res.1 = action_button(ui, ok_text, 150.0, true).clicked();
            res.0 = action_button(ui, "Отмена", 100.0, false).clicked();
        });
    });
    res
}

// ---------- виджеты и рисование ----------

fn menu_entry(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).size(13.5)).frame(false)
}

fn menu_item(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(menu_entry(label))
}

fn power_caption(snap: &Snapshot) -> (String, Color32) {
    match &snap.power {
        Power::On => match snap.public_addr {
            Some(a) => (format!("в сети · внешний {a}"), col::MUTED),
            None => ("в сети · узнаём внешний адрес".into(), col::MUTED),
        },
        Power::Starting => ("запуск…".into(), col::AMBER),
        Power::Off => ("выключен · нажмите клавишу питания".into(), col::DIM),
        Power::Failed(_) => ("ошибка запуска".into(), col::RED),
    }
}

/// Клавиша питания: квадрат с контуром, знак питания и светодиод в углу.
fn power_key(ui: &mut Ui, power: &Power) -> egui::Response {
    let size = 68.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let resp = resp.on_hover_text(match power {
        Power::On | Power::Starting => "Выключить",
        _ => "Включить",
    });
    let p = ui.painter();
    let pressed = resp.is_pointer_button_down_on();
    let fill = if pressed {
        Color32::BLACK
    } else if resp.hovered() {
        col::RAISED
    } else {
        col::PANEL
    };
    p.rect_filled(rect, CornerRadius::same(2), fill);
    let border = if resp.hovered() { col::MUTED } else { col::LINE2 };
    p.rect_stroke(rect, CornerRadius::same(2), Stroke::new(1.0, border), StrokeKind::Inside);

    let t = ui.input(|i| i.time);
    let blink = (t * 3.0) as i64 % 2 == 0;
    let (glyph, led) = match power {
        Power::On => (col::ACCENT, col::ACCENT),
        Power::Starting => (col::TEXT, if blink { col::AMBER } else { col::DIM }),
        Power::Failed(_) => (col::RED, col::RED),
        Power::Off => (col::MUTED, Color32::from_rgb(0x2a, 0x2a, 0x28)),
    };
    if *power == Power::Starting {
        ui.ctx().request_repaint_after(Duration::from_millis(150));
    }
    let led_rect = Rect::from_min_size(pos2(rect.right() - 13.0, rect.top() + 7.0), vec2(6.0, 6.0));
    p.rect_filled(led_rect, CornerRadius::ZERO, led);

    let c = rect.center() + vec2(0.0, 2.0);
    let ir = 13.0;
    let gap = 40f32.to_radians();
    let start = -std::f32::consts::FRAC_PI_2 + gap;
    let end = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU - gap;
    let pts: Vec<Pos2> = (0..=48)
        .map(|i| {
            let a = start + (end - start) * i as f32 / 48.0;
            c + vec2(a.cos(), a.sin()) * ir
        })
        .collect();
    let stroke = Stroke::new(2.0, glyph);
    p.add(egui::Shape::line(pts, stroke));
    p.line_segment([c + vec2(0.0, -ir - 3.0), c + vec2(0.0, -2.0)], stroke);
    resp
}

fn paint_triangle(p: &Painter, c: Pos2, collapsed: bool) {
    let s = 4.0;
    let pts = if collapsed {
        vec![c + vec2(-s * 0.6, -s), c + vec2(s * 0.7, 0.0), c + vec2(-s * 0.6, s)]
    } else {
        vec![c + vec2(-s, -s * 0.6), c + vec2(s, -s * 0.6), c + vec2(0.0, s * 0.7)]
    };
    p.add(egui::Shape::convex_polygon(pts, col::DIM, Stroke::NONE));
}

// ---------- демо-режим (для скриншотов и показа) ----------

fn demo_snapshot() -> Snapshot {
    use service::MemberState::*;
    let m = |id, name: &str, ip: [u8; 2], state, addr: Option<&str>| MemberView {
        id,
        name: name.into(),
        vip: std::net::Ipv4Addr::new(10, 77, ip[0], ip[1]),
        state,
        addr: addr.map(|a| a.parse().unwrap()),
    };
    let ms = |v| Some(Duration::from_millis(v));
    Snapshot {
        power: Power::On,
        nick: "Алекс".into(),
        vip: std::net::Ipv4Addr::new(10, 77, 12, 34),
        public_addr: Some("84.32.63.75:47777".parse().unwrap()),
        upnp: None,
        vpn_warning: false,
        settings: Settings::default(),
        networks: vec![
            NetworkView {
                name: "Друзья".into(),
                password: "minecraft2026".into(),
                public_addr: Some("84.32.63.75:47777".parse().unwrap()),
                searching: 0,
                members: vec![
                    m(1, "Вася", [31, 7], Direct { rtt: ms(24) }, Some("95.24.11.80:47777")),
                    m(2, "Петя", [140, 2], Direct { rtt: ms(118) }, Some("178.66.4.19:51022")),
                    m(3, "Даша", [77, 150], Relay { via: "Вася".into() }, None),
                    m(4, "Маша", [9, 201], Offline, None),
                ],
            },
            NetworkView {
                name: "Майнкрафт сервер".into(),
                password: "creeper-42".into(),
                public_addr: Some("84.32.63.75:47778".parse().unwrap()),
                searching: 0,
                members: vec![
                    m(5, "Server-PC", [200, 1], Direct { rtt: ms(12) }, Some("31.173.80.5:47777")),
                    m(6, "Игорь", [56, 88], Offline, None),
                ],
            },
            NetworkView {
                name: "Турнир по CS".into(),
                password: "headshot".into(),
                public_addr: None,
                searching: 3,
                members: vec![],
            },
        ],
    }
}

fn demo_dialog(name: &str, snap: &Snapshot) -> Option<Dialog> {
    Some(match name {
        "create" => Dialog::Create {
            name: "Друзья".into(),
            password: "secret".into(),
            confirm: "secr".into(),
            error: Some("Пароли не совпадают".into()),
        },
        "join" => Dialog::Join { name: String::new(), password: String::new(), error: None },
        "invite" => Dialog::Invite { network: snap.networks.first()?.name.clone() },
        "settings" => Dialog::Settings { nick: snap.nick.clone(), settings: snap.settings.clone(), autostart: true },
        "leave" => Dialog::Leave { network: snap.networks.first()?.name.clone() },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_invite_with_and_without_newlines() {
        let snap = demo_snapshot();
        let text = invite_text(&snap.networks[0], &snap);
        let want = Some(("Друзья".to_string(), "minecraft2026".to_string()));
        assert_eq!(parse_invite(&text), want);
        assert_eq!(parse_invite(&text.replace('\n', "")), want);
        assert_eq!(parse_invite(&text.replace('\n', " ")), want);
        assert_eq!(parse_invite("просто название"), None);
    }

    #[test]
    fn mark_icon_has_transparent_corners_and_opaque_center() {
        let n = 32;
        let img = mark_icon(n, IconState::On);
        assert_eq!(img.len(), n * n * 4);
        assert_eq!(img[3], 0, "угол прозрачный");
        let center = (n / 2 * n + n / 2) * 4;
        assert_eq!(img[center + 3], 255, "центр непрозрачный");
    }
}

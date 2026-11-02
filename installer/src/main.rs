//! Установщик и деинсталлятор P2PJack.
//!
//! `P2PJack-Setup.exe` — установка или обновление; копия в папке программы `uninstall.exe --uninstall`
//! удаляет её (её запускает «Приложения» Windows).
//!
//! Скрытые флаги для проверки внешнего вида: `--no-elevate`, `--screenshot файл.png`,
//! `--demo-screen options|progress|done|uninstall`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result, bail};
use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Ui, vec2};
use p2pjack::platform;
use p2pjack::ui_kit::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static APP_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/p2pjack.exe.gz"));
static WINTUN_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wintun.dll.gz"));

const APP_NAME: &str = "P2PJack";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\P2PJack";

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let uninstall = has("--uninstall") || value("--demo-screen").as_deref() == Some("uninstall");

    // Program Files, реестр и Планировщик требуют прав администратора.
    #[cfg(windows)]
    if !has("--no-elevate")
        && !platform::is_elevated()
        && platform::relaunch_elevated(if uninstall { "--uninstall" } else { "" })
    {
        return Ok(());
    }

    let title = if uninstall { "Удаление P2PJack" } else { "Установка P2PJack" };
    let icon = mark_icon(64, IconState::On);
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([520.0, 480.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_maximize_button(false)
            .with_icon(Arc::new(egui::IconData { rgba: icon, width: 64, height: 64 })),
        ..Default::default()
    };
    let screenshot = value("--screenshot").map(PathBuf::from);
    let demo = value("--demo-screen");
    eframe::run_native(
        title,
        native,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            Ok(Box::new(Setup::new(uninstall, demo.as_deref(), screenshot)))
        }),
    )
}

// ---------- состояние ----------

#[derive(Clone, Copy, PartialEq)]
enum StepState {
    Pending,
    Running,
    Done,
    Skipped,
    Failed,
}

struct Step {
    name: &'static str,
    state: StepState,
}

#[derive(Default)]
struct Progress {
    steps: Vec<Step>,
    finished: bool,
    error: Option<String>,
}

type Task = Box<dyn FnOnce() -> Result<bool> + Send>;

#[derive(PartialEq)]
enum Screen {
    Options,
    Working,
    Done,
}

struct Setup {
    uninstall: bool,
    /// Деинсталлятор запущен, а P2PJack не установлен — удалять нечего.
    not_installed: bool,
    /// Уже установлен — тогда это обновление.
    upgrade: bool,
    screen: Screen,
    dir: String,
    desktop: bool,
    autostart: bool,
    launch: bool,
    wipe_config: bool,
    progress: Arc<Mutex<Progress>>,
    /// Папка, которую надо удалить после выхода (в ней лежит сам деинсталлятор).
    remove_after_exit: Option<PathBuf>,
    shot: Option<(PathBuf, u32, Arc<AtomicBool>)>,
}

impl Setup {
    fn new(uninstall: bool, demo: Option<&str>, screenshot: Option<PathBuf>) -> Self {
        let installed = installed_dir();
        // Удаляем только то, что действительно установлено: папку из списка программ или ту,
        // где лежит сам uninstall.exe. Никогда — произвольную папку, из которой запустили exe.
        let dir = if uninstall {
            installed.clone().or_else(own_install_dir)
        } else {
            Some(installed.clone().unwrap_or_else(default_dir))
        };
        let not_installed = uninstall && dir.is_none() && demo.is_none();
        let dir = dir.unwrap_or_else(default_dir);
        let mut setup = Self {
            uninstall,
            not_installed,
            upgrade: installed.is_some(),
            screen: Screen::Options,
            dir: dir.to_string_lossy().into_owned(),
            desktop: true,
            autostart: installed.is_none() || platform::autostart_enabled(),
            launch: true,
            wipe_config: false,
            progress: Arc::new(Mutex::new(Progress::default())),
            remove_after_exit: None,
            shot: screenshot.map(|p| (p, 0, Arc::new(AtomicBool::new(false)))),
        };
        if let Some(screen) = demo {
            setup.demo(screen);
        }
        setup
    }

    /// Состояния для скриншотов без реальной установки.
    fn demo(&mut self, screen: &str) {
        let names = install_step_names();
        let states: &[StepState] = match screen {
            "progress" => &[StepState::Done, StepState::Done, StepState::Running, StepState::Pending, StepState::Pending],
            "done" => &[StepState::Done, StepState::Done, StepState::Done, StepState::Done, StepState::Done],
            _ => return,
        };
        let mut p = self.progress.lock().unwrap();
        p.steps = names.iter().zip(states).map(|(&name, &state)| Step { name, state }).collect();
        p.finished = screen == "done";
        drop(p);
        self.screen = if screen == "done" { Screen::Done } else { Screen::Working };
    }

    fn start(&mut self, ctx: &egui::Context) {
        let dir = PathBuf::from(self.dir.trim());
        let tasks: Vec<(&'static str, Task)> = if self.uninstall {
            let wipe = self.wipe_config;
            let me_inside = std::env::current_exe().is_ok_and(|p| p.parent() == Some(dir.as_path()));
            if me_inside {
                self.remove_after_exit = Some(dir.clone());
            }
            vec![
                ("Закрываем P2PJack", Box::new(close_running_app)),
                ("Отключаем автозапуск", Box::new(|| platform::set_autostart(false, Path::new("")).map(|_| true))),
                ("Убираем ярлыки", Box::new(|| remove_shortcuts().map(|_| true))),
                ("Убираем правило брандмауэра", Box::new(remove_firewall_rule)),
                ("Убираем из списка программ", Box::new(|| registry::delete_tree(UNINSTALL_KEY).map(|_| true))),
                ("Удаляем файлы", Box::new(move || remove_files(&dir, me_inside))),
                ("Сети и настройки", Box::new(move || wipe_settings(wipe))),
            ]
        } else {
            let (desktop, autostart) = (self.desktop, self.autostart);
            let names = install_step_names();
            let (d1, d2, d3, d4) = (dir.clone(), dir.clone(), dir.clone(), dir.clone());
            vec![
                (names[0], Box::new(close_running_app)),
                (names[1], Box::new(move || copy_files(&d1))),
                (names[2], Box::new(move || create_shortcuts(&d2, desktop).map(|_| true))),
                (names[3], Box::new(move || platform::set_autostart(autostart, &d3.join("p2pjack.exe")).map(|_| autostart))),
                (names[4], Box::new(move || register(&d4).map(|_| true))),
            ]
        };
        self.screen = Screen::Working;
        let progress = self.progress.clone();
        progress.lock().unwrap().steps =
            tasks.iter().map(|(name, _)| Step { name, state: StepState::Pending }).collect();
        let (ctx, launch) = (ctx.clone(), (!self.uninstall && self.launch).then(|| PathBuf::from(self.dir.trim())));
        std::thread::spawn(move || {
            for (i, (_, task)) in tasks.into_iter().enumerate() {
                progress.lock().unwrap().steps[i].state = StepState::Running;
                ctx.request_repaint();
                let r = task();
                let mut p = progress.lock().unwrap();
                match r {
                    Ok(true) => p.steps[i].state = StepState::Done,
                    Ok(false) => p.steps[i].state = StepState::Skipped,
                    Err(e) => {
                        p.steps[i].state = StepState::Failed;
                        p.error = Some(format!("{e:#}"));
                        p.finished = true;
                        ctx.request_repaint();
                        return;
                    }
                }
            }
            if let Some(dir) = launch {
                let _ = std::process::Command::new(dir.join("p2pjack.exe")).current_dir(&dir).spawn();
            }
            progress.lock().unwrap().finished = true;
            ctx.request_repaint();
        });
    }
}

fn install_step_names() -> [&'static str; 5] {
    ["Закрываем запущенный P2PJack", "Копируем файлы", "Создаём ярлыки", "Автозапуск", "Добавляем в список программ"]
}

// ---------- окно ----------

impl eframe::App for Setup {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Пока идёт установка, окно не закрываем.
        if self.screen == Screen::Working && self.shot.is_none() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let finished = self.progress.lock().unwrap().finished;
        if self.screen == Screen::Working && finished && self.progress.lock().unwrap().error.is_none() {
            self.screen = Screen::Done;
        }

        title_bar(ui, false, |ui| {
            ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).font(mono(11.0)).color(col::DIM));
        });
        egui::CentralPanel::default()
            .frame(Frame::new().fill(col::BG).inner_margin(Margin::symmetric(28, 20)))
            .show(ui, |ui| {
                ui.add_space(6.0);
                match self.screen {
                    Screen::Options => self.options(ui),
                    Screen::Working => self.working(ui),
                    Screen::Done => self.done(ui),
                }
            });
        window_frame(&ctx, false);
        self.screenshot_step(&ctx);
        if self.screen == Screen::Working {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Деинсталлятор лежит в удаляемой папке: удалить её можно только после выхода.
        if let Some(dir) = &self.remove_after_exit {
            remove_dir_after_exit(dir);
        }
    }
}

impl Setup {
    fn options(&mut self, ui: &mut Ui) {
        if self.not_installed {
            ui.label(caps("Удаление", 11.0, col::RED));
            ui.label(RichText::new("P2PJack не установлен").font(display(30.0)).color(col::TEXT));
            ui.label(RichText::new("В списке программ его нет — удалять нечего.").color(col::MUTED));
            let bottom = ui.max_rect().bottom();
            let rect = egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), bottom - 36.0), ui.max_rect().right_bottom());
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action_button(ui, "Закрыть", 140.0, false).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            return;
        }
        if self.uninstall {
            ui.label(caps("Удаление", 11.0, col::RED));
            ui.label(RichText::new("Удалить P2PJack?").font(display(30.0)).color(col::TEXT));
            ui.label(
                RichText::new("Программа отключится от всех сетей, исчезнут ярлыки и автозапуск.")
                    .color(col::MUTED),
            );
            ui.add_space(14.0);
            ui.label(caps("Папка", 10.5, col::DIM));
            ui.label(RichText::new(&self.dir).font(mono(12.5)).color(col::TEXT));
            ui.add_space(14.0);
            ui.checkbox(&mut self.wipe_config, "Удалить и мои сети с паролями, и настройки");
            self.bottom_buttons(ui, "Удалить", col::RED);
            return;
        }
        ui.label(caps(if self.upgrade { "Обновление" } else { "Установка" }, 11.0, col::ACCENT));
        ui.label(RichText::new("P2PJack").font(display(30.0)).color(col::TEXT));
        ui.label(
            RichText::new("Виртуальная локальная сеть для игр. Компьютеры соединяются напрямую — без сервера.")
                .color(col::MUTED),
        );
        ui.add_space(14.0);
        ui.label(caps("Папка", 10.5, col::DIM));
        ui.add_enabled(
            !self.upgrade,
            egui::TextEdit::singleline(&mut self.dir)
                .font(mono(12.5))
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(10, 8)),
        );
        if self.upgrade {
            ui.label(RichText::new("уже установлен — файлы будут заменены, сети сохранятся").font(mono(11.0)).color(col::DIM));
        }
        ui.add_space(10.0);
        ui.checkbox(&mut self.desktop, "Ярлык на рабочем столе");
        ui.checkbox(&mut self.autostart, "Запускать вместе с Windows (сразу в трей)");
        ui.checkbox(&mut self.launch, "Открыть P2PJack после установки");
        let label = if self.upgrade { "Обновить" } else { "Установить" };
        self.bottom_buttons(ui, label, col::ACCENT);
    }

    fn bottom_buttons(&mut self, ui: &mut Ui, label: &str, color: Color32) {
        let bottom = ui.max_rect().bottom();
        let rect = egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), bottom - 36.0), ui.max_rect().right_bottom());
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let go = ui.add(
                    egui::Button::new(caps(label, 12.5, Color32::BLACK))
                        .fill(color)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(1))
                        .min_size(vec2(160.0, 36.0)),
                );
                if go.clicked() {
                    if !self.uninstall && self.dir.trim().is_empty() {
                        return;
                    }
                    let ctx = ui.ctx().clone();
                    self.start(&ctx);
                }
                if action_button(ui, "Отмена", 110.0, false).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    fn working(&mut self, ui: &mut Ui) {
        let p = self.progress.lock().unwrap();
        let failed = p.error.is_some();
        let (label, color) = if failed {
            ("Ошибка", col::RED)
        } else if self.uninstall {
            ("Удаление", col::RED)
        } else {
            ("Установка", col::ACCENT)
        };
        ui.label(caps(label, 11.0, color));
        let title = if failed { "Не получилось" } else if self.uninstall { "Удаляем…" } else { "Устанавливаем…" };
        ui.label(RichText::new(title).font(display(30.0)).color(col::TEXT));
        ui.add_space(14.0);
        steps_table(ui, &p.steps);
        if let Some(err) = &p.error {
            ui.add_space(10.0);
            ui.label(RichText::new(format!("× {err}")).font(mono(12.0)).color(col::RED));
            drop(p);
            let bottom = ui.max_rect().bottom();
            let rect = egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), bottom - 36.0), ui.max_rect().right_bottom());
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action_button(ui, "Закрыть", 140.0, false).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }
    }

    fn done(&mut self, ui: &mut Ui) {
        let dir = self.dir.trim().to_string();
        if self.uninstall {
            ui.label(caps("Готово", 11.0, col::ACCENT));
            ui.label(RichText::new("P2PJack удалён").font(display(30.0)).color(col::TEXT));
            ui.label(RichText::new("Спасибо, что играли по сети с P2PJack.").color(col::MUTED));
        } else {
            ui.label(caps("Готово", 11.0, col::ACCENT));
            ui.label(RichText::new(if self.upgrade { "Обновлено" } else { "Установлено" }).font(display(30.0)).color(col::TEXT));
            let note = if self.launch {
                "P2PJack запущен. Создайте сеть и отправьте приглашение друзьям."
            } else {
                "P2PJack — в меню «Пуск»."
            };
            ui.label(RichText::new(note).color(col::MUTED));
        }
        ui.add_space(14.0);
        steps_table(ui, &self.progress.lock().unwrap().steps);
        ui.add_space(8.0);
        if !self.uninstall {
            ui.label(RichText::new(&dir).font(mono(11.5)).color(col::DIM));
        }
        let bottom = ui.max_rect().bottom();
        let rect = egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), bottom - 36.0), ui.max_rect().right_bottom());
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if action_button(ui, "Готово", 160.0, true).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some((path, frame, done)) = &mut self.shot else { return };
        *frame += 1;
        ctx.request_repaint();
        if *frame == 10 {
            let (path, done) = (path.clone(), done.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                if let Err(e) = save_png(&path, &img) {
                    eprintln!("скриншот: {e}");
                }
                done.store(true, Ordering::SeqCst);
            })));
        }
        if done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Шаги таблицей: квадратик состояния, название, итог.
fn steps_table(ui: &mut Ui, steps: &[Step]) {
    let blink = (ui.input(|i| i.time) * 3.0) as i64 % 2 == 0;
    for step in steps {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
        let p = ui.painter();
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, col::LINE));
        let mark = egui::Rect::from_center_size(egui::pos2(rect.left() + 4.0, rect.center().y), egui::Vec2::splat(7.0));
        let (fill, status, status_color, text_color) = match step.state {
            StepState::Pending => (None, "", col::DIM, col::DIM),
            StepState::Running => (Some(if blink { col::AMBER } else { col::DIM }), "…", col::AMBER, col::TEXT),
            StepState::Done => (Some(col::ACCENT), "ок", col::ACCENT, col::TEXT),
            StepState::Skipped => (Some(col::DIM), "пропуск", col::DIM, col::MUTED),
            StepState::Failed => (Some(col::RED), "ошибка", col::RED, col::TEXT),
        };
        match fill {
            Some(c) => {
                p.rect_filled(mark, CornerRadius::ZERO, c);
            }
            None => {
                p.rect_stroke(mark, CornerRadius::ZERO, Stroke::new(1.0, col::DIM), StrokeKind::Inside);
            }
        }
        let y = rect.center().y;
        text(p, egui::pos2(rect.left() + 20.0, y), egui::Align2::LEFT_CENTER, step.name, body(14.0), text_color, rect.width() - 100.0);
        text(p, egui::pos2(rect.right(), y), egui::Align2::RIGHT_CENTER, status, mono(12.0), status_color, 80.0);
    }
}

// ---------- шаги установки ----------

fn default_dir() -> PathBuf {
    let base = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    base.join(APP_NAME)
}

/// Куда P2PJack установлен раньше (по записи в списке программ).
fn installed_dir() -> Option<PathBuf> {
    registry::get_string(UNINSTALL_KEY, "InstallLocation")
        .map(PathBuf::from)
        .filter(|d| d.join("p2pjack.exe").is_file())
}

fn unpack(gz: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz).read_to_end(&mut out)?;
    Ok(out)
}

/// Просит запущенный P2PJack выйти (он отключится от сетей и уберёт адаптер) и ждёт этого.
fn close_running_app() -> Result<bool> {
    if !platform::signal_instance("quit") {
        return Ok(false);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if std::net::UdpSocket::bind(("127.0.0.1", platform::INSTANCE_PORT)).is_ok() {
            // Порт свободен — программа вышла; даём ОС отпустить файлы.
            std::thread::sleep(Duration::from_millis(700));
            return Ok(true);
        }
    }
    let _ = platform::command("taskkill").args(["/F", "/IM", "p2pjack.exe"]).output();
    std::thread::sleep(Duration::from_millis(700));
    Ok(true)
}

fn write_replacing(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, data).with_context(|| format!("не удалось записать {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("не удалось заменить {} (файл занят?)", path.display()))?;
    Ok(())
}

fn copy_files(dir: &Path) -> Result<bool> {
    let app = unpack(APP_GZ)?;
    if app.is_empty() {
        bail!("установщик собран без программы — соберите его скриптом scripts\\build-installer.ps1");
    }
    std::fs::create_dir_all(dir).with_context(|| format!("не удалось создать {}", dir.display()))?;
    write_replacing(&dir.join("p2pjack.exe"), &app)?;
    let wintun = unpack(WINTUN_GZ)?;
    let dll = dir.join("wintun.dll");
    // Если драйвер не вложен или занят, программа скачает или использует имеющийся.
    if !wintun.is_empty() && write_replacing(&dll, &wintun).is_err() && !dll.is_file() {
        bail!("не удалось записать {}", dll.display());
    }
    let me = std::env::current_exe()?;
    let uninstaller = dir.join("uninstall.exe");
    if me.canonicalize().ok() != uninstaller.canonicalize().ok() {
        std::fs::copy(&me, &uninstaller).with_context(|| format!("не удалось записать {}", uninstaller.display()))?;
    }
    Ok(true)
}

fn start_menu_link() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{APP_NAME}.lnk"))
}

fn desktop_link() -> PathBuf {
    let base = std::env::var_os("PUBLIC").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Users\Public"));
    base.join("Desktop").join(format!("{APP_NAME}.lnk"))
}

/// Ярлыки для всех пользователей: в «Пуске» и, если выбрано, на рабочем столе.
fn create_shortcuts(dir: &Path, desktop: bool) -> Result<()> {
    let q = |p: &Path| p.to_string_lossy().replace('\'', "''");
    let exe = dir.join("p2pjack.exe");
    let mut links = vec![start_menu_link()];
    if desktop {
        links.push(desktop_link());
    } else {
        let _ = std::fs::remove_file(desktop_link());
    }
    let mut script = String::from("$ErrorActionPreference = 'Stop'; $ws = New-Object -ComObject WScript.Shell; ");
    for link in &links {
        script += &format!(
            "$s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; \
             $s.Description = 'Виртуальная локальная сеть для игр'; $s.Save(); ",
            q(link),
            q(&exe),
            q(dir)
        );
    }
    let out = platform::command("powershell").args(["-NoProfile", "-Command", &script]).output()?;
    if !out.status.success() {
        bail!("не удалось создать ярлыки: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn remove_shortcuts() -> Result<()> {
    for link in [start_menu_link(), desktop_link()] {
        if link.exists() {
            std::fs::remove_file(&link).with_context(|| format!("не удалось удалить {}", link.display()))?;
        }
    }
    Ok(())
}

/// Запись в «Приложения» / «Программы и компоненты».
fn register(dir: &Path) -> Result<()> {
    let size_kb = ["p2pjack.exe", "wintun.dll", "uninstall.exe"]
        .iter()
        .filter_map(|f| std::fs::metadata(dir.join(f)).ok())
        .map(|m| m.len() / 1024)
        .sum::<u64>() as u32;
    let d = dir.to_string_lossy();
    registry::set_values(
        UNINSTALL_KEY,
        &[
            ("DisplayName", registry::Value::Str(APP_NAME.into())),
            ("DisplayVersion", registry::Value::Str(env!("CARGO_PKG_VERSION").into())),
            ("Publisher", registry::Value::Str(APP_NAME.into())),
            ("DisplayIcon", registry::Value::Str(format!("{d}\\p2pjack.exe"))),
            ("InstallLocation", registry::Value::Str(d.to_string())),
            ("UninstallString", registry::Value::Str(format!("\"{d}\\uninstall.exe\" --uninstall"))),
            ("EstimatedSize", registry::Value::Dword(size_kb)),
            ("NoModify", registry::Value::Dword(1)),
            ("NoRepair", registry::Value::Dword(1)),
        ],
    )
}

fn remove_firewall_rule() -> Result<bool> {
    let out = platform::command("netsh").args(["advfirewall", "firewall", "delete", "rule", "name=P2PJack"]).output()?;
    Ok(out.status.success())
}

/// Файлы, которые кладёт установщик. Удаляются только они, чужие файлы в папке не трогаем.
const INSTALLED_FILES: [&str; 6] =
    ["p2pjack.exe", "wintun.dll", "p2pjack-cli.exe", "p2pjack.new", "wintun.new", "uninstall.exe"];

/// Папка, где лежит запущенный `uninstall.exe` вместе с `p2pjack.exe`.
fn own_install_dir() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let named_uninstall = me.file_name()?.eq_ignore_ascii_case("uninstall.exe");
    let dir = me.parent()?.to_path_buf();
    (named_uninstall && dir.join("p2pjack.exe").is_file()).then_some(dir)
}

fn remove_files(dir: &Path, me_inside: bool) -> Result<bool> {
    for f in INSTALLED_FILES {
        // Запущенный деинсталлятор удалится после выхода.
        if me_inside && f == "uninstall.exe" {
            continue;
        }
        let path = dir.join(f);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("не удалось удалить {} (файл занят?)", path.display()))?;
        }
    }
    if !me_inside {
        // Только пустую: вдруг P2PJack поставили в общую папку.
        let _ = std::fs::remove_dir(dir);
    }
    Ok(true)
}

fn wipe_settings(wipe: bool) -> Result<bool> {
    let dir = p2pjack::config::config_dir();
    if !wipe {
        return Ok(false);
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("не удалось удалить {}", dir.display()))?;
    }
    Ok(true)
}

/// Удаляет деинсталлятор и пустую папку через пару секунд после выхода (пока он работает, exe занят).
fn remove_dir_after_exit(dir: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let d = dir.display();
        let cmd = format!(r#"/c ping -n 3 127.0.0.1 >nul & del /f /q "{d}\uninstall.exe" & rmdir "{d}""#);
        let _ = platform::command("cmd").raw_arg(cmd).spawn();
    }
    #[cfg(not(windows))]
    let _ = dir;
}

// ---------- реестр ----------

#[cfg(windows)]
mod registry {
    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
    };

    pub enum Value {
        Str(String),
        Dword(u32),
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Создаёт ключ в HKLM и записывает значения.
    pub fn set_values(key: &str, values: &[(&str, Value)]) -> Result<()> {
        set_values_in(HKEY_LOCAL_MACHINE, key, values)
    }

    pub fn set_values_in(root: HKEY, key: &str, values: &[(&str, Value)]) -> Result<()> {
        let mut h: HKEY = std::ptr::null_mut();
        let path = wide(key);
        let r = unsafe {
            RegCreateKeyExW(
                root,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut h,
                std::ptr::null_mut(),
            )
        };
        if r != ERROR_SUCCESS {
            bail!("реестр: не удалось открыть {key} (код {r})");
        }
        let mut result = Ok(());
        for (name, value) in values {
            let name_w = wide(name);
            let r = match value {
                Value::Str(s) => {
                    let data = wide(s);
                    unsafe {
                        RegSetValueExW(h, name_w.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
                    }
                }
                Value::Dword(v) => unsafe {
                    RegSetValueExW(h, name_w.as_ptr(), 0, REG_DWORD, (v as *const u32).cast(), 4)
                },
            };
            if r != ERROR_SUCCESS {
                result = Err(anyhow::anyhow!("реестр: не удалось записать {name} (код {r})"));
                break;
            }
        }
        unsafe { RegCloseKey(h) };
        result
    }

    pub fn get_string(key: &str, value: &str) -> Option<String> {
        get_string_in(HKEY_LOCAL_MACHINE, key, value)
    }

    pub fn get_string_in(root: HKEY, key: &str, value: &str) -> Option<String> {
        let (key_w, value_w) = (wide(key), wide(value));
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                root,
                key_w.as_ptr(),
                value_w.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// Удаляет ключ со всем содержимым; отсутствие ключа — не ошибка.
    pub fn delete_tree(key: &str) -> Result<()> {
        delete_tree_in(HKEY_LOCAL_MACHINE, key)
    }

    pub fn delete_tree_in(root: HKEY, key: &str) -> Result<()> {
        let key_w = wide(key);
        let r = unsafe { RegDeleteTreeW(root, key_w.as_ptr()) };
        const ERROR_FILE_NOT_FOUND: u32 = 2;
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            bail!("реестр: не удалось удалить {key} (код {r})");
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

        /// Пишет, читает и удаляет временный ключ в HKCU.
        #[test]
        fn registry_roundtrip_in_current_user() {
            let key = format!(r"Software\P2PJack-selftest-{}", std::process::id());
            set_values_in(
                HKEY_CURRENT_USER,
                &key,
                &[("Path", Value::Str(r#""C:\Program Files\P2PJack\uninstall.exe" --uninstall"#.into())), ("N", Value::Dword(7))],
            )
            .unwrap();
            let read = get_string_in(HKEY_CURRENT_USER, &key, "Path");
            delete_tree_in(HKEY_CURRENT_USER, &key).unwrap();
            assert_eq!(read.as_deref(), Some(r#""C:\Program Files\P2PJack\uninstall.exe" --uninstall"#));
            assert_eq!(get_string_in(HKEY_CURRENT_USER, &key, "Path"), None);
        }
    }
}

#[cfg(not(windows))]
mod registry {
    pub enum Value {
        Str(String),
        Dword(u32),
    }
    pub fn set_values(_: &str, _: &[(&str, Value)]) -> anyhow::Result<()> {
        anyhow::bail!("установщик работает только на Windows")
    }
    pub fn get_string(_: &str, _: &str) -> Option<String> {
        None
    }
    pub fn delete_tree(_: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_payload_and_removes_only_own_files() {
        if unpack(APP_GZ).unwrap().is_empty() {
            eprintln!("установщик собран без программы — пропускаем");
            return;
        }
        let dir = std::env::temp_dir().join(format!("p2pjack-setup-test-{}", std::process::id()));
        copy_files(&dir).unwrap();
        let exe = std::fs::read(dir.join("p2pjack.exe")).unwrap();
        assert_eq!(&exe[..2], b"MZ");
        assert!(dir.join("uninstall.exe").is_file());

        let foreign = dir.join("чужой файл.txt");
        std::fs::write(&foreign, "не трогать").unwrap();
        remove_files(&dir, false).unwrap();
        assert!(!dir.join("p2pjack.exe").exists());
        assert!(!dir.join("uninstall.exe").exists());
        assert!(foreign.exists(), "чужой файл удалён");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! P2PJack с графическим интерфейсом.
//!
//! `--autostart` — запуск из автозагрузки Windows: окно не показывается, программа сразу в трее.
//!
//! Скрытые флаги для проверки внешнего вида без драйвера и прав администратора:
//! `--demo` (выдуманные сети), `--demo-dialog create|join|invite|settings|leave`,
//! `--screenshot файл.png` (сохранить окно и выйти), `--splash-at мс` (кадр заставки),
//! `--selftest-tray` (спрятать в трей и вернуть),
//! `--no-elevate`.

// В release нет консольного окна; в debug оно остаётся для журнала.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;
mod splash;

fn main() -> eframe::Result {
    let mut opts = gui::Options::from_args();
    // Программа уже работает (возможно, свёрнута в трей). При ручном запуске показываем её окно,
    // при автозапуске молча выходим.
    if !opts.demo && p2pjack::platform::signal_instance(if opts.autostart { "ping" } else { "show" }) {
        return Ok(());
    }
    // Сетевой адаптер создаётся только с правами администратора — просим их сразу, как Radmin VPN.
    #[cfg(windows)]
    if !opts.demo && !opts.no_elevate && !p2pjack::platform::is_elevated() && p2pjack::platform::relaunch_elevated("") {
        return Ok(());
    }
    // Из автозагрузки — сразу в трей, если пользователь не отключил трей.
    opts.start_hidden = opts.autostart && p2pjack::config::Config::load().settings.tray;

    let logs = gui::Logs::default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "p2pjack=info".into()),
        )
        .with_target(false)
        .with_ansi(false)
        .without_time()
        .with_writer(logs.make_writer())
        .init();

    gui::run(opts, logs)
}
